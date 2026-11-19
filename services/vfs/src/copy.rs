// SPDX-License-Identifier: Apache-2.0
//! The copy engine (TASK-105A, FR-085), in vfs. A job copies or moves files
//! and folders into a folder, a step at a time (\`step\`, each up to a byte
//! budget), so vfs answers everyone else between steps; the caller (Files'
//! copy dialog) steps it while it runs, and pausing is not stepping.
//!
//! Its ways, after Freally File Manager's (its ideas, not its code):
//!
//! - in one subvolume a copy is a **reflink** (the blocks shared, nothing
//!   written but the new file's records) and a move is a **rename**;
//! - otherwise bytes go through a 1 MiB buffer (read whole, then written),
//!   the modified time kept, a move removing its source once copied;
//! - **verify** (optional) hashes what's read and reads the copy back to
//!   compare (so it never takes the reflink, whose blocks are the source's);
//! - a name already taken is a **conflict**: the job waits for Replace,
//!   Skip or Keep both, for this one or all; *Let me decide for each file*
//!   is \`conflicts\` (every one ahead) and a choice per file;
//! - a failure waits for Try again, Skip or Cancel; Cancel removes the
//!   file it was part-way through.
//!
//! Many small files go through in one step (the budget counts files too):
//! vfs is one thread, so "parallel" here is batching, and the time a step
//! takes is what the dialog's speed graph shows.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;

/// What's at a path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    pub dir: bool,
    pub size: u64,
    pub modified_ns: u64,
}

pub type Said = String;

/// What the engine needs of the file system (vfs over ffs; RAM in tests).
pub trait Tree {
    /// What's at `path` (`None`: nothing).
    fn meta(&mut self, path: &str) -> Result<Option<Meta>, Said>;
    fn list(&mut self, path: &str) -> Result<Vec<String>, Said>;
    fn mkdir(&mut self, path: &str) -> Result<(), Said>;
    /// `from` copied to `to` sharing its blocks: false when they aren't in
    /// one subvolume (nothing done).
    fn reflink(&mut self, from: &str, to: &str) -> Result<bool, Said>;
    fn read(&mut self, path: &str, offset: u64, len: u64) -> Result<Vec<u8>, Said>;
    /// Writes at `offset`; at 0 the file is made (or emptied) first.
    fn write(&mut self, path: &str, offset: u64, bytes: &[u8]) -> Result<(), Said>;
    fn set_modified(&mut self, path: &str, ns: u64) -> Result<(), Said>;
    /// A move in one subvolume: false when they aren't (nothing done).
    fn rename(&mut self, from: &str, to: &str) -> Result<bool, Said>;
    /// A file, or an empty folder.
    fn remove(&mut self, path: &str) -> Result<(), Said>;
}

/// The buffer: 1 MiB (File Manager's default).
pub const BUFFER: u64 = 1 << 20;
/// A step's file budget: many small files go in one step, not one each.
const STEP_FILES: u32 = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Copy,
    Move,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Replace,
    Skip,
    KeepBoth,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub src: String,
    pub dst: String,
    pub src_meta: Meta,
    pub dst_meta: Meta,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Running,
    /// Waiting for a choice.
    Conflict(Conflict),
    /// Waiting for Try again, Skip or Cancel: the path and what went wrong.
    Failed(String, Said),
    Done,
    Cancelled,
}

#[derive(Clone)]
enum Work {
    /// A file or folder not yet looked at.
    Item { src: String, dst: String },
    /// A file going through the buffer: how far, and the hash so far.
    Bytes { src: String, dst: String, at: u64, meta: Meta, hash: Option<fcrypto::sha2::Sha256> },
    /// A copy read back to check it (verify): how far, what it must hash
    /// to, and its hash so far; a buffer a step, like the copy itself.
    Verify { src: String, dst: String, at: u64, meta: Meta, want: [u8; 32], back: fcrypto::sha2::Sha256 },
    /// A folder whose contents are done: its time kept; a move removes it.
    DirDone { src: String, dst: String, modified_ns: u64 },
}

pub struct Job {
    pub mode: Mode,
    pub verify: bool,
    pub state: State,
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub done_items: u64,
    pub total_items: u64,
    /// What it's working on (a source path).
    pub current: String,
    /// Files copied by reflink (their blocks shared).
    pub reflinked: u64,
    queue: VecDeque<Work>,
    /// A choice for every conflict (\`Replace all\`, \`Skip all\`, \`Keep all\`).
    all: Option<Choice>,
    /// A choice per source path (\`Let me decide for each file\`).
    each: BTreeMap<String, Choice>,
}

fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') { alloc::format!("{dir}{name}") } else { alloc::format!("{dir}/{name}") }
}

fn split(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(0) => ("/", &path[1..]),
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    }
}

/// `name (2)`, `name (3)` … (before a file's extension) that isn't taken.
pub fn free_name(t: &mut dyn Tree, dir: &str, name: &str, dir_kind: bool) -> Result<String, Said> {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && !dir_kind => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for n in 2..10_000 {
        let candidate = alloc::format!("{stem} ({n}){ext}");
        if t.meta(&join(dir, &candidate))?.is_none() {
            return Ok(candidate);
        }
    }
    Err(String::from("No free name is left for it."))
}

/// Every file and folder under `path` (itself included): (items, bytes).
fn weigh(t: &mut dyn Tree, path: &str) -> Result<(u64, u64), Said> {
    let m = t.meta(path)?.ok_or_else(|| alloc::format!("{path} isn't there."))?;
    if !m.dir {
        return Ok((1, m.size));
    }
    let mut total = (1, 0);
    for name in t.list(path)? {
        let (i, b) = weigh(t, &join(path, &name))?;
        total = (total.0 + i, total.1 + b);
    }
    Ok(total)
}

impl Job {
    /// A job: `sources` copied (or moved) into folder `dest`. Checked and
    /// weighed first (the dialog's totals).
    pub fn new(t: &mut dyn Tree, sources: &[String], dest: &str, mode: Mode, verify: bool) -> Result<Job, Said> {
        match t.meta(dest)? {
            Some(m) if m.dir => {}
            _ => return Err(alloc::format!("{dest} isn't a folder.")),
        }
        if sources.is_empty() {
            return Err(String::from("There's nothing to copy."));
        }
        let mut job = Job {
            mode,
            verify,
            state: State::Running,
            done_bytes: 0,
            total_bytes: 0,
            done_items: 0,
            total_items: 0,
            current: String::new(),
            reflinked: 0,
            queue: VecDeque::new(),
            all: None,
            each: BTreeMap::new(),
        };
        for src in sources {
            if dest == src || dest.starts_with(&alloc::format!("{src}/")) {
                return Err(String::from("A folder can't go inside itself."));
            }
            let (items, bytes) = weigh(t, src)?;
            job.total_items += items;
            job.total_bytes += bytes;
            let (from_dir, name) = split(src);
            // Into the folder it's in: a copy gets a free name; a move is nothing.
            if from_dir == dest {
                if mode == Mode::Move {
                    job.done_items += items;
                    job.done_bytes += bytes;
                    continue;
                }
                let dir = t.meta(src)?.is_some_and(|m| m.dir);
                let name = free_name(t, dest, name, dir)?;
                job.queue.push_back(Work::Item { src: src.clone(), dst: join(dest, &name) });
            } else {
                job.queue.push_back(Work::Item { src: src.clone(), dst: join(dest, name) });
            }
        }
        Ok(job)
    }

    /// Up to `budget` bytes (and `STEP_FILES` files) done; the state after.
    pub fn step(&mut self, t: &mut dyn Tree, budget: u64) -> &State {
        if self.state != State::Running {
            return &self.state;
        }
        let (mut bytes, mut files) = (0u64, 0u32);
        while bytes < budget && files < STEP_FILES {
            let Some(work) = self.queue.pop_front() else {
                self.state = State::Done;
                break;
            };
            match self.work(t, work, budget - bytes) {
                Ok((b, f)) => {
                    bytes += b;
                    files += f;
                }
                Err(w) => {
                    if self.state == State::Running {
                        self.state = State::Failed(self.current.clone(), w);
                    }
                    break;
                }
            }
            if self.state != State::Running {
                break;
            }
        }
        &self.state
    }

    /// One piece of work; it goes back on the queue's front if it isn't
    /// finished (a file part-way, a conflict, a failure). (bytes, files) done.
    fn work(&mut self, t: &mut dyn Tree, work: Work, room: u64) -> Result<(u64, u32), Said> {
        match work {
            Work::Item { src, dst } => {
                self.current = src.clone();
                let Some(m) = t.meta(&src)? else {
                    self.queue.push_front(Work::Item { src: src.clone(), dst });
                    return Err(alloc::format!("{src} isn't there any more."));
                };
                let mut dst = dst;
                if let Some(d) = t.meta(&dst)? {
                    if m.dir && d.dir {
                        // A folder into one of the same name: their contents merge.
                    } else {
                        match self.each.get(&src).copied().or(self.all) {
                            None => {
                                self.state = State::Conflict(Conflict {
                                    src: src.clone(),
                                    dst: dst.clone(),
                                    src_meta: m,
                                    dst_meta: d,
                                });
                                self.queue.push_front(Work::Item { src, dst });
                                return Ok((0, 0));
                            }
                            Some(Choice::Skip) => {
                                let (i, b) = weigh(t, &src)?;
                                self.done_items += i;
                                self.done_bytes += b;
                                return Ok((0, 1));
                            }
                            Some(Choice::Replace) => {
                                if d.dir {
                                    remove_all(t, &dst)?;
                                } else {
                                    t.remove(&dst)?;
                                }
                            }
                            Some(Choice::KeepBoth) => {
                                let (dir, name) = split(&dst);
                                let dir = String::from(dir);
                                let name = free_name(t, &dir, name, m.dir)?;
                                dst = join(&dir, &name);
                            }
                        }
                    }
                }
                if self.mode == Mode::Move && t.meta(&dst)?.is_none() && t.rename(&src, &dst)? {
                    let (i, b) = weigh(t, &dst)?;
                    self.done_items += i;
                    self.done_bytes += b;
                    return Ok((b, 1));
                }
                if m.dir {
                    if t.meta(&dst)?.is_none() {
                        t.mkdir(&dst)?;
                    }
                    self.queue.push_front(Work::DirDone {
                        src: src.clone(),
                        dst: dst.clone(),
                        modified_ns: m.modified_ns,
                    });
                    let mut names = t.list(&src)?;
                    names.sort();
                    for n in names.into_iter().rev() {
                        self.queue.push_front(Work::Item { src: join(&src, &n), dst: join(&dst, &n) });
                    }
                    self.done_items += 1;
                    return Ok((0, 1));
                }
                if !self.verify && t.reflink(&src, &dst)? {
                    self.reflinked += 1;
                    self.done_items += 1;
                    self.done_bytes += m.size;
                    if self.mode == Mode::Move {
                        t.remove(&src)?;
                    }
                    return Ok((m.size, 1));
                }
                let hash = self.verify.then(fcrypto::sha2::Sha256::new);
                self.queue.push_front(Work::Bytes { src, dst, at: 0, meta: m, hash });
                Ok((0, 0))
            }
            Work::Bytes { src, dst, at, meta, mut hash } => {
                self.current = src.clone();
                let n = BUFFER.min(meta.size - at).min(room.max(1));
                let done = (|| {
                    let bytes = if n == 0 { Vec::new() } else { t.read(&src, at, n)? };
                    if bytes.len() as u64 != n {
                        return Err(alloc::format!("{src} changed while it was copied."));
                    }
                    t.write(&dst, at, &bytes)?;
                    if let Some(h) = hash.as_mut() {
                        h.update(&bytes);
                    }
                    Ok(())
                })();
                if let Err(e) = done {
                    self.queue.push_front(Work::Bytes { src, dst, at, meta, hash });
                    return Err(e);
                }
                self.done_bytes += n;
                let at = at + n;
                if at < meta.size {
                    self.queue.push_front(Work::Bytes { src, dst, at, meta, hash });
                    return Ok((n, 0));
                }
                if let Some(h) = hash.take() {
                    let back = fcrypto::sha2::Sha256::new();
                    self.queue.push_front(Work::Verify { src, dst, at: 0, meta, want: h.finish(), back });
                    return Ok((n, 0));
                }
                self.finish_file(t, &src, &dst, &meta)?;
                Ok((n, 1))
            }
            Work::Verify { src, dst, at, meta, want, mut back } => {
                self.current = src.clone();
                let n = BUFFER.min(meta.size - at).min(room.max(1));
                let b = match if n == 0 { Ok(Vec::new()) } else { t.read(&dst, at, n) } {
                    Ok(b) => b,
                    Err(e) => {
                        self.queue.push_front(Work::Verify { src, dst, at, meta, want, back });
                        return Err(e);
                    }
                };
                back.update(&b);
                let read = b.len() as u64;
                if at + read < meta.size && read > 0 {
                    self.queue.push_front(Work::Verify { src, dst, at: at + read, meta, want, back });
                    return Ok((read, 0));
                }
                if back.finish() != want {
                    self.queue.push_front(Work::Item { src: src.clone(), dst: dst.clone() });
                    t.remove(&dst)?;
                    return Err(alloc::format!("The copy of {} doesn't match it.", split(&src).1));
                }
                self.finish_file(t, &src, &dst, &meta)?;
                Ok((read, 1))
            }
            Work::DirDone { src, dst, modified_ns } => {
                t.set_modified(&dst, modified_ns)?;
                if self.mode == Mode::Move {
                    t.remove(&src)?;
                }
                Ok((0, 0))
            }
        }
    }

    /// A file copied (and checked): its time kept, counted, and a move's
    /// source removed.
    fn finish_file(&mut self, t: &mut dyn Tree, src: &str, dst: &str, meta: &Meta) -> Result<(), Said> {
        t.set_modified(dst, meta.modified_ns)?;
        self.done_items += 1;
        if self.mode == Mode::Move {
            t.remove(src)?;
        }
        Ok(())
    }

    /// The conflict answered: for this one, or (`all`) for every one.
    pub fn decide(&mut self, choice: Choice, all: bool) {
        let State::Conflict(c) = &self.state else { return };
        if all {
            self.all = Some(choice);
        } else {
            self.each.insert(c.src.clone(), choice);
        }
        self.state = State::Running;
    }

    /// *Let me decide for each file*: every conflict still ahead (its
    /// top-level items: a folder's contents merge).
    pub fn conflicts(&mut self, t: &mut dyn Tree) -> Result<Vec<Conflict>, Said> {
        let mut out = Vec::new();
        for w in &self.queue {
            if let Work::Item { src, dst } = w
                && !self.each.contains_key(src)
                && let (Some(s), Some(d)) = (t.meta(src)?, t.meta(dst)?)
                && !(s.dir && d.dir)
            {
                out.push(Conflict { src: src.clone(), dst: dst.clone(), src_meta: s, dst_meta: d });
            }
        }
        Ok(out)
    }

    /// A choice per file, from *Let me decide for each file*.
    pub fn decide_each(&mut self, choices: &[(String, Choice)]) {
        for (src, c) in choices {
            self.each.insert(src.clone(), *c);
        }
        if matches!(self.state, State::Conflict(_)) {
            self.state = State::Running;
        }
    }

    /// After a failure: Try again (the same work), or Skip it.
    pub fn retry(&mut self, skip: bool, t: &mut dyn Tree) {
        if !matches!(self.state, State::Failed(..)) {
            return;
        }
        if skip && let Some(w) = self.queue.pop_front() {
            match w {
                Work::Bytes { src, dst, at, meta, .. } => {
                    let _ = t.remove(&dst);
                    let _ = src;
                    self.done_bytes += meta.size - at;
                    self.done_items += 1;
                }
                // A copy that couldn't be checked isn't kept.
                Work::Verify { dst, .. } => {
                    let _ = t.remove(&dst);
                    self.done_items += 1;
                }
                Work::Item { src, .. } => {
                    let (i, b) = weigh(t, &src).unwrap_or((1, 0));
                    self.done_items += i;
                    self.done_bytes += b;
                }
                Work::DirDone { .. } => {}
            }
        }
        self.state = State::Running;
    }

    /// Stopped: the file part-way through removed (a copy never leaves half a file).
    pub fn cancel(&mut self, t: &mut dyn Tree) {
        if let Some(Work::Bytes { dst, .. } | Work::Verify { dst, .. }) = self.queue.front() {
            let _ = t.remove(dst);
        }
        self.queue.clear();
        self.state = State::Cancelled;
    }
}

fn remove_all(t: &mut dyn Tree, path: &str) -> Result<(), Said> {
    if t.meta(path)?.is_some_and(|m| m.dir) {
        for n in t.list(path)? {
            remove_all(t, &join(path, &n))?;
        }
    }
    t.remove(path)
}

#[cfg(test)]
mod tests;
