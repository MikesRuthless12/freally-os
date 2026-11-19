// SPDX-License-Identifier: Apache-2.0
//! vfs, the service (TASK-100, FR-043): the file protocol for every
//! process, by path, through ffs. Each request is checked against the
//! rules in the library (the caller conductor named, and the session's
//! account): `/system` read-only, each home its account's alone. Open
//! files are handles with a place, per connection; so are copy jobs
//! (TASK-105A, `svc_vfs::copy`), stepped by their caller, each path they
//! touch checked as the caller's.
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::{FErr, signal};
use fstd::ipc::{Channel, Client};
use fstd::time::{Duration, Instant};
use fwire::proto::conductor as c;
use fwire::proto::ffs as f;
use fwire::proto::vfs as v;
use svc_vfs::copy::{self, Choice, Job, Meta, Mode, Said, State, Tree};
use svc_vfs::{Access, Caller, Place, check, homes_listed, parse};

fstd::program!(run);

/// The system subvolume and every subvolume's root directory.
const SYSTEM: u64 = 256;
const ROOT_INO: u64 = 256;

type Reply = Result<Vec<u8>, (FErr, String)>;

/// An open file: where it is, its place, whether it was opened to write.
struct Open {
    id: u32,
    sv: u64,
    ino: u64,
    at: u64,
    write: bool,
}

struct Conn {
    who: String,
    ch: Channel,
    files: Vec<Open>,
    next: u32,
    /// Copy jobs, by id.
    jobs: Vec<(u32, Job)>,
}

/// What a copy step may do: 4 MiB (or 256 small files), then vfs answers
/// everyone else.
const STEP_BYTES: u64 = 4 << 20;
/// What one ffs message carries.
const CHUNK: u64 = 60 * 1024;

/// The copy engine's view of the files: every path checked as `who`'s.
struct Files<'a> {
    vfs: &'a mut Vfs,
    who: &'a str,
}

fn said(e: (FErr, String)) -> Said {
    e.1
}

impl Tree for Files<'_> {
    fn meta(&mut self, path: &str) -> Result<Option<Meta>, Said> {
        match self.vfs.resolve(self.who, path, Access::Read) {
            Ok((sv, ino, _)) => {
                let s: f::Stat = self.vfs.ffs(f::STAT, &f::Ino { sv, ino }).map_err(said)?;
                Ok(Some(Meta { dir: s.kind == f::DIR, size: s.size, modified_ns: s.modified_ns }))
            }
            Err((FErr::NotFound, _)) => Ok(None),
            Err(e) => Err(said(e)),
        }
    }
    fn list(&mut self, path: &str) -> Result<Vec<String>, Said> {
        let (sv, ino, _) = self.vfs.resolve(self.who, path, Access::Read).map_err(said)?;
        let es: f::Entries = self.vfs.ffs(f::READDIR, &f::Ino { sv, ino }).map_err(said)?;
        Ok(es.entries.into_iter().map(|(n, _)| n).collect())
    }
    fn mkdir(&mut self, path: &str) -> Result<(), Said> {
        let (sv, dir, name) = self.vfs.parent(self.who, path).map_err(said)?;
        self.vfs.ffs::<_, f::Entry>(f::CREATE, &f::Create { sv, dir, name, kind: f::DIR }).map(|_| ()).map_err(said)
    }
    fn reflink(&mut self, from: &str, to: &str) -> Result<bool, Said> {
        let (sv, src, _) = self.vfs.resolve(self.who, from, Access::Read).map_err(said)?;
        let (to_sv, dir, name) = self.vfs.parent(self.who, to).map_err(said)?;
        if sv != to_sv {
            return Ok(false);
        }
        self.vfs.ffs::<_, f::Entry>(f::CLONE, &f::CloneFile { sv, src, dir, name }).map(|_| true).map_err(said)
    }
    fn read(&mut self, path: &str, offset: u64, len: u64) -> Result<Vec<u8>, Said> {
        let (sv, ino, _) = self.vfs.resolve(self.who, path, Access::Read).map_err(said)?;
        let mut out = Vec::with_capacity(len as usize);
        while (out.len() as u64) < len {
            let at = offset + out.len() as u64;
            let n = CHUNK.min(len - out.len() as u64) as u32;
            let d: f::Data = self.vfs.ffs(f::READ, &f::Read { sv, ino, offset: at, len: n }).map_err(said)?;
            if d.bytes.is_empty() {
                break;
            }
            out.extend_from_slice(&d.bytes);
        }
        Ok(out)
    }
    fn write(&mut self, path: &str, offset: u64, bytes: &[u8]) -> Result<(), Said> {
        let (sv, ino) = match self.vfs.resolve(self.who, path, Access::Write) {
            Ok((sv, ino, _)) => {
                if offset == 0 {
                    self.vfs.ffs::<_, ()>(f::TRUNCATE, &f::Truncate { sv, ino, size: 0 }).map_err(said)?;
                }
                (sv, ino)
            }
            Err((FErr::NotFound, _)) if offset == 0 => {
                let (sv, dir, name) = self.vfs.parent(self.who, path).map_err(said)?;
                let e: f::Entry = self.vfs.ffs(f::CREATE, &f::Create { sv, dir, name, kind: f::FILE }).map_err(said)?;
                (sv, e.ino)
            }
            Err(e) => return Err(said(e)),
        };
        for (i, chunk) in bytes.chunks(CHUNK as usize).enumerate() {
            let at = offset + i as u64 * CHUNK;
            self.vfs.ffs::<_, ()>(f::WRITE, &f::Write { sv, ino, offset: at, bytes: chunk.to_vec() }).map_err(said)?;
        }
        Ok(())
    }
    fn set_modified(&mut self, path: &str, ns: u64) -> Result<(), Said> {
        let (sv, ino, _) = self.vfs.resolve(self.who, path, Access::Write).map_err(said)?;
        self.vfs.ffs::<_, ()>(f::SET_MODIFIED, &f::Times { sv, ino, modified_ns: ns }).map_err(said)
    }
    fn rename(&mut self, from: &str, to: &str) -> Result<bool, Said> {
        let (sv, dir, name) = self.vfs.parent(self.who, from).map_err(said)?;
        let (to_sv, to_dir, to_name) = self.vfs.parent(self.who, to).map_err(said)?;
        if sv != to_sv {
            return Ok(false);
        }
        self.vfs.ffs::<_, ()>(f::RENAME, &f::Rename { sv, dir, name, to_dir, to_name }).map(|_| true).map_err(said)
    }
    fn remove(&mut self, path: &str) -> Result<(), Said> {
        let (sv, dir, name) = self.vfs.parent(self.who, path).map_err(said)?;
        self.vfs.ffs::<_, ()>(f::UNLINK, &f::Name { sv, dir, name }).map_err(said)
    }
}

/// Where a job stands, as the protocol says it.
fn state_of(job: &Job) -> v::CopyState {
    let mut conflict = v::CopyConflict {
        src: String::new(),
        dst: String::new(),
        src_size: 0,
        src_modified_ns: 0,
        dst_size: 0,
        dst_modified_ns: 0,
    };
    let (state, words) = match &job.state {
        State::Running => (v::COPY_RUNNING, String::new()),
        State::Conflict(c) => {
            conflict = wire_conflict(c);
            (v::COPY_CONFLICT, String::new())
        }
        State::Failed(path, w) => (v::COPY_FAILED, alloc::format!("{path}: {w}")),
        State::Done => (v::COPY_DONE, String::new()),
        State::Cancelled => (v::COPY_CANCELLED, String::new()),
    };
    v::CopyState {
        state,
        done_items: job.done_items,
        total_items: job.total_items,
        done_bytes: job.done_bytes,
        total_bytes: job.total_bytes,
        current: job.current.clone(),
        conflict,
        words,
    }
}

fn wire_conflict(c: &copy::Conflict) -> v::CopyConflict {
    v::CopyConflict {
        src: c.src.clone(),
        dst: c.dst.clone(),
        src_size: c.src_meta.size,
        src_modified_ns: c.src_meta.modified_ns,
        dst_size: c.dst_meta.size,
        dst_modified_ns: c.dst_meta.modified_ns,
    }
}

fn choice(c: u8) -> Option<Choice> {
    match c {
        v::CHOOSE_REPLACE => Some(Choice::Replace),
        v::CHOOSE_SKIP => Some(Choice::Skip),
        v::CHOOSE_KEEP_BOTH => Some(Choice::KeepBoth),
        _ => None,
    }
}

struct Vfs {
    ffs: Client,
    /// The session's account and its home's subvolume.
    session: Option<(String, u64)>,
}

fn denied(e: FErr) -> (FErr, String) {
    let why = match e {
        FErr::AccessDenied => "not yours to reach",
        FErr::InvalidArgs => "not a path vfs takes (absolute, no . or ..)",
        FErr::NotFound => "no such place",
        _ => "refused",
    };
    (e, String::from(why))
}

impl Vfs {
    fn ffs<Req: fwire::Wire, Rep: fwire::Wire>(&mut self, ordinal: u32, req: &Req) -> Result<Rep, (FErr, String)> {
        self.ffs
            .call::<_, Rep>(ordinal, req, Vec::new(), Instant::after(Duration::from_secs(10)))
            .map(|(r, _)| r)
            .map_err(|e| match e {
                fstd::ipc::CallError::Remote(code, text) => (FErr::from_code(code as i64), text),
                other => (FErr::Internal, alloc::format!("ffs: {other:?}")),
            })
    }

    fn caller<'a>(&'a self, who: &'a str) -> Caller<'a> {
        Caller { service: who, session: self.session.as_ref().map(|s| s.0.as_str()) }
    }

    /// The subvolume and the directory a place's components start from.
    fn base(&self, place: &Place) -> Result<(u64, u64, Vec<String>), (FErr, String)> {
        match place {
            Place::System(parts) => Ok((SYSTEM, ROOT_INO, parts.clone())),
            Place::Home(_, parts) => {
                let sv = self.session.as_ref().map(|s| s.1).ok_or(denied(FErr::AccessDenied))?;
                Ok((sv, ROOT_INO, parts.clone()))
            }
            Place::Root | Place::Homes => Err((FErr::WrongType, String::from("a directory of mounts"))),
        }
    }

    /// The inode a path names, and its kind.
    fn resolve(&mut self, who: &str, path: &str, access: Access) -> Result<(u64, u64, u8), (FErr, String)> {
        let place = parse(path).map_err(denied)?;
        check(&self.caller(who), &place, access).map_err(denied)?;
        let (sv, mut ino, parts) = self.base(&place)?;
        let mut kind = f::DIR;
        for name in parts {
            if kind != f::DIR {
                return Err((FErr::WrongType, String::from("a file isn't a directory")));
            }
            let e: f::Entry = self.ffs(f::LOOKUP, &f::Name { sv, dir: ino, name })?;
            (ino, kind) = (e.ino, e.kind);
        }
        Ok((sv, ino, kind))
    }

    /// The directory holding a path's last component, and that name.
    fn parent(&mut self, who: &str, path: &str) -> Result<(u64, u64, String), (FErr, String)> {
        let place = parse(path).map_err(denied)?;
        check(&self.caller(who), &place, Access::Write).map_err(denied)?;
        let (sv, root, mut parts) = self.base(&place)?;
        let name = parts.pop().ok_or((FErr::AccessDenied, String::from("a mount can't be made or removed")))?;
        let mut dir = root;
        for p in parts {
            let e: f::Entry = self.ffs(f::LOOKUP, &f::Name { sv, dir, name: p })?;
            if e.kind != f::DIR {
                return Err((FErr::WrongType, String::from("a file isn't a directory")));
            }
            dir = e.ino;
        }
        Ok((sv, dir, name))
    }

    fn handle(&mut self, conn: &mut Conn, ordinal: u32, p: &[u8]) -> Reply {
        fn arg<T: fwire::Wire>(p: &[u8]) -> Result<T, (FErr, String)> {
            fwire::from_payload(p).map_err(|e| (FErr::InvalidArgs, String::from(e.describe())))
        }
        let unit = || Ok(fwire::to_payload(&()));
        let who = conn.who.clone();
        match ordinal {
            v::OPEN => {
                let o: v::Open = arg(p)?;
                let write = o.write || o.create || o.truncate;
                let access = if write { Access::Write } else { Access::Read };
                let (sv, ino) = match self.resolve(&who, &o.path, access) {
                    Ok((_, _, k)) if k == f::DIR => {
                        return Err((FErr::WrongType, String::from("a directory can't be opened")));
                    }
                    Ok((sv, ino, _)) => (sv, ino),
                    Err((FErr::NotFound, _)) if o.create => {
                        let (sv, dir, name) = self.parent(&who, &o.path)?;
                        let e: f::Entry = self.ffs(f::CREATE, &f::Create { sv, dir, name, kind: f::FILE })?;
                        (sv, e.ino)
                    }
                    Err(e) => return Err(e),
                };
                if o.truncate {
                    self.ffs::<_, ()>(f::TRUNCATE, &f::Truncate { sv, ino, size: 0 })?;
                }
                let st: f::Stat = self.ffs(f::STAT, &f::Ino { sv, ino })?;
                let id = conn.next;
                conn.next += 1;
                conn.files.push(Open { id, sv, ino, at: 0, write });
                Ok(fwire::to_payload(&v::Opened { handle: id, size: st.size }))
            }
            v::READ => {
                let r: v::ReadAt = arg(p)?;
                let o = conn.files.iter_mut().find(|o| o.id == r.handle).ok_or(denied(FErr::BadHandle))?;
                let (sv, ino, at) = (o.sv, o.ino, o.at);
                let d: f::Data = self.ffs(f::READ, &f::Read { sv, ino, offset: at, len: r.len.min(60 * 1024) })?;
                if let Some(o) = conn.files.iter_mut().find(|o| o.id == r.handle) {
                    o.at += d.bytes.len() as u64;
                }
                Ok(fwire::to_payload(&v::Data { bytes: d.bytes }))
            }
            v::WRITE => {
                let w: v::WriteAt = arg(p)?;
                let o = conn.files.iter().find(|o| o.id == w.handle).ok_or(denied(FErr::BadHandle))?;
                if !o.write {
                    return Err((FErr::AccessDenied, String::from("opened to read")));
                }
                let (sv, ino, at, n) = (o.sv, o.ino, o.at, w.bytes.len() as u64);
                self.ffs::<_, ()>(f::WRITE, &f::Write { sv, ino, offset: at, bytes: w.bytes })?;
                if let Some(o) = conn.files.iter_mut().find(|o| o.id == w.handle) {
                    o.at += n;
                }
                unit()
            }
            v::SEEK => {
                let s: v::Seek = arg(p)?;
                let o = conn.files.iter_mut().find(|o| o.id == s.handle).ok_or(denied(FErr::BadHandle))?;
                o.at = s.to;
                unit()
            }
            v::CLOSE => {
                let h: v::Handle = arg(p)?;
                conn.files.retain(|o| o.id != h.handle);
                unit()
            }
            v::STAT => {
                let path: v::Path = arg(p)?;
                let place = parse(&path.path).map_err(denied)?;
                if matches!(place, Place::Root | Place::Homes) {
                    check(&self.caller(&who), &place, Access::Read).map_err(denied)?;
                    return Ok(fwire::to_payload(&v::Stat { dir: true, size: 2, modified_ns: 0, created_ns: 0 }));
                }
                let (sv, ino, _) = self.resolve(&who, &path.path, Access::Read)?;
                let s: f::Stat = self.ffs(f::STAT, &f::Ino { sv, ino })?;
                Ok(fwire::to_payload(&v::Stat {
                    dir: s.kind == f::DIR,
                    size: s.size,
                    modified_ns: s.modified_ns,
                    created_ns: s.created_ns,
                }))
            }
            v::READDIR => {
                let path: v::Path = arg(p)?;
                let place = parse(&path.path).map_err(denied)?;
                let entries = match place {
                    Place::Root => alloc::vec![(String::from("home"), true), (String::from("system"), true)],
                    Place::Homes => homes_listed(&self.caller(&who)).into_iter().map(|u| (u, true)).collect(),
                    _ => {
                        let (sv, ino, kind) = self.resolve(&who, &path.path, Access::Read)?;
                        if kind != f::DIR {
                            return Err((FErr::WrongType, String::from("a file isn't a directory")));
                        }
                        let es: f::Entries = self.ffs(f::READDIR, &f::Ino { sv, ino })?;
                        let mut out: Vec<(String, bool)> =
                            es.entries.into_iter().map(|(n, e)| (n, e.kind == f::DIR)).collect();
                        out.sort();
                        out
                    }
                };
                Ok(fwire::to_payload(&v::Entries { entries }))
            }
            v::MKDIR => {
                let path: v::Path = arg(p)?;
                let (sv, dir, name) = self.parent(&who, &path.path)?;
                self.ffs::<_, f::Entry>(f::CREATE, &f::Create { sv, dir, name, kind: f::DIR })?;
                unit()
            }
            v::UNLINK => {
                let path: v::Path = arg(p)?;
                let (sv, dir, name) = self.parent(&who, &path.path)?;
                self.ffs::<_, ()>(f::UNLINK, &f::Name { sv, dir, name })?;
                unit()
            }
            v::RENAME => {
                let r: v::Rename = arg(p)?;
                let (sv, dir, name) = self.parent(&who, &r.from)?;
                let (to_sv, to_dir, to_name) = self.parent(&who, &r.to)?;
                if sv != to_sv {
                    return Err((
                        FErr::NotSupported,
                        String::from("a move between subvolumes is a copy (the copy engine)"),
                    ));
                }
                self.ffs::<_, ()>(f::RENAME, &f::Rename { sv, dir, name, to_dir, to_name })?;
                unit()
            }
            v::FSYNC => {
                self.ffs::<_, ()>(f::FSYNC, &())?;
                unit()
            }
            v::SNAPSHOT => {
                let s: v::Snapshot = arg(p)?;
                let (user, sv) = self.session.clone().ok_or(denied(FErr::AccessDenied))?;
                if who == "accounts" {
                    return Err(denied(FErr::AccessDenied));
                }
                let name = alloc::format!("snapshot:{user}:{}", s.name);
                self.ffs::<_, f::Subvol>(f::SNAPSHOT, &f::Snapshot { sv, name })?;
                unit()
            }
            v::WHOAMI => {
                Ok(fwire::to_payload(&v::Who { user: self.session.as_ref().map(|s| s.0.clone()).unwrap_or_default() }))
            }
            v::SET_SESSION => {
                let s: v::Session = arg(p)?;
                let allowed = who == "accounts" || (cfg!(feature = "flavour-e2e") && who == "conductor");
                if !allowed {
                    return Err((FErr::AccessDenied, String::from("only accounts sets the session")));
                }
                self.session = (!s.user.is_empty()).then_some((s.user.clone(), s.sv));
                fstd::info!("vfs: session {}", if s.user.is_empty() { "ended" } else { s.user.as_str() });
                unit()
            }
            v::COPY_START => {
                let c: v::CopyStart = arg(p)?;
                let mode = if c.moving { Mode::Move } else { Mode::Copy };
                let job = Job::new(&mut Files { vfs: self, who: &who }, &c.sources, &c.dest, mode, c.verify)
                    .map_err(|w| (FErr::InvalidArgs, w))?;
                let id = conn.next;
                conn.next += 1;
                let reply = v::CopyJob { job: id, total_items: job.total_items, total_bytes: job.total_bytes };
                fstd::info!(
                    "vfs: {} {id} for {who}: {} item(s), {} bytes into {}",
                    if c.moving { "move" } else { "copy" },
                    job.total_items,
                    job.total_bytes,
                    c.dest
                );
                conn.jobs.push((id, job));
                Ok(fwire::to_payload(&reply))
            }
            v::COPY_STEP | v::COPY_CONFLICTS | v::COPY_DECIDE | v::COPY_EACH | v::COPY_CANCEL => {
                let id = match ordinal {
                    v::COPY_DECIDE => arg::<v::CopyDecide>(p)?.job,
                    v::COPY_EACH => arg::<v::CopyEach>(p)?.job,
                    _ => arg::<v::CopyRef>(p)?.job,
                };
                let i = conn.jobs.iter().position(|(j, _)| *j == id).ok_or(denied(FErr::BadHandle))?;
                let job = &mut conn.jobs[i].1;
                let mut t = Files { vfs: self, who: &who };
                let out = match ordinal {
                    v::COPY_STEP => {
                        job.step(&mut t, STEP_BYTES);
                        fwire::to_payload(&state_of(job))
                    }
                    v::COPY_CONFLICTS => {
                        let all = job.conflicts(&mut t).map_err(|w| (FErr::Internal, w))?;
                        fwire::to_payload(&v::CopyConflicts { items: all.iter().map(wire_conflict).collect() })
                    }
                    v::COPY_DECIDE => {
                        let d: v::CopyDecide = arg(p)?;
                        match (&job.state, choice(d.choice)) {
                            (State::Failed(..), _) => job.retry(d.choice != v::CHOOSE_TRY_AGAIN, &mut t),
                            (State::Conflict(_), Some(c)) => job.decide(c, d.all),
                            _ => return Err((FErr::BadState, String::from("nothing to decide"))),
                        }
                        fwire::to_payload(&())
                    }
                    v::COPY_EACH => {
                        let e: v::CopyEach = arg(p)?;
                        let each: Vec<(String, Choice)> =
                            e.choices.into_iter().filter_map(|(path, c)| choice(c).map(|c| (path, c))).collect();
                        job.decide_each(&each);
                        fwire::to_payload(&())
                    }
                    _ => {
                        job.cancel(&mut t);
                        fwire::to_payload(&())
                    }
                };
                if matches!(conn.jobs[i].1.state, State::Done | State::Cancelled) {
                    let (id, job) = conn.jobs.remove(i);
                    fstd::info!(
                        "vfs: copy {id} {}: {} of {} item(s), {} bytes, {} by reflink",
                        if job.state == State::Done { "done" } else { "cancelled" },
                        job.done_items,
                        job.total_items,
                        job.done_bytes,
                        job.reflinked
                    );
                }
                Ok(out)
            }
            _ => Err((FErr::NotSupported, String::from("vfs doesn't serve that"))),
        }
    }
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    let ffs = match fstd::service::connect_within("ffs", Duration::from_secs(30)) {
        Ok(ch) => Client::new(ch),
        // No storage on this machine (ffs said why): nothing to serve.
        Err(_) => {
            fstd::info!("vfs: no storage: no files to serve");
            return 0;
        }
    };
    let mut vfs = Vfs { ffs, session: None };
    let Ok((listen, give)) = Channel::pair() else { return 1 };
    if fstd::service::register(give).is_err() {
        return 1;
    }
    fstd::info!("vfs: / with /system and /home");
    let mut conns: Vec<Conn> = Vec::new();
    loop {
        let mut items = alloc::vec![[listen.0.raw(), signal::READABLE]];
        items.extend(conns.iter().map(|c| [c.ch.0.raw(), signal::READABLE | signal::PEER_CLOSED]));
        match fstd::sys::wait_many(&items, u64::MAX) {
            Ok(0) => {
                if let Ok(m) = listen.recv(0)
                    && let Ok((h, p)) = fwire::parse(&m.bytes)
                    && h.ordinal == c::INCOMING
                    && let Ok(inc) = fwire::from_payload::<c::Incoming>(p)
                    && let Some(ch) = m.handles.into_iter().next()
                {
                    conns.push(Conn { who: inc.client, ch: Channel(ch), files: Vec::new(), next: 1, jobs: Vec::new() });
                }
            }
            Ok(k) => {
                if !serve(&mut vfs, &mut conns[k - 1]) {
                    // Gone with copies part-way: each stopped, no half file left.
                    let mut gone = conns.remove(k - 1);
                    for (_, mut job) in gone.jobs.drain(..) {
                        job.cancel(&mut Files { vfs: &mut vfs, who: &gone.who });
                    }
                }
            }
            Err(_) => fstd::time::sleep(Duration::from_millis(10)),
        }
    }
}

/// One connection's requests until its channel is empty; false when it's gone.
fn serve(vfs: &mut Vfs, conn: &mut Conn) -> bool {
    loop {
        let m = match conn.ch.try_recv() {
            Ok(m) => m,
            Err(FErr::ShouldWait) => return true,
            Err(_) => return false,
        };
        if fstd::inventory::seen(&conn.ch, &m.bytes) {
            continue;
        }
        let Ok((h, p)) = fwire::parse(&m.bytes) else { continue };
        let msg = match vfs.handle(conn, h.ordinal, p) {
            Ok(body) => fwire::message_bytes(h.txid, h.ordinal, fwire::flags::REPLY, body),
            Err((e, text)) => fwire::error_reply(h.txid, h.ordinal, e as i32, &text),
        };
        if let Ok(msg) = msg {
            let _ = conn.ch.send(&msg, Vec::new());
        }
    }
}
