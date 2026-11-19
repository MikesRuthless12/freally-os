// SPDX-License-Identifier: Apache-2.0
//! The copy engine on a tree in RAM with two subvolumes (`/system/…` and
//! `/home/…`): reflinks and renames only inside one.

use super::*;
use alloc::string::ToString;
use alloc::vec;

#[derive(Default)]
struct Mem {
    /// Path → (bytes, or None for a folder; modified).
    items: BTreeMap<String, (Option<Vec<u8>>, u64)>,
    reflinks: u32,
    renames: u32,
    /// The next read of this path fails (once).
    fail_read: Option<String>,
    /// Every write flips a byte (to catch with verify).
    corrupt: bool,
    /// Bytes read so far.
    read_bytes: u64,
}

fn sv(path: &str) -> &str {
    if path.starts_with("/system") { "system" } else { "home" }
}

impl Mem {
    fn with(files: &[(&str, &[u8])], dirs: &[&str]) -> Mem {
        let mut m = Mem::default();
        for d in ["/system", "/home", "/home/m"].iter().chain(dirs) {
            m.items.insert(d.to_string(), (None, 7));
        }
        for (p, b) in files {
            m.items.insert(p.to_string(), (Some(b.to_vec()), 1234));
        }
        m
    }
    fn bytes(&self, p: &str) -> Option<&[u8]> {
        self.items.get(p).and_then(|(b, _)| b.as_deref())
    }
}

impl Tree for Mem {
    fn meta(&mut self, path: &str) -> Result<Option<Meta>, Said> {
        Ok(self.items.get(path).map(|(b, m)| Meta {
            dir: b.is_none(),
            size: b.as_ref().map_or(0, |b| b.len() as u64),
            modified_ns: *m,
        }))
    }
    fn list(&mut self, path: &str) -> Result<Vec<String>, Said> {
        let pre = alloc::format!("{path}/");
        Ok(self
            .items
            .keys()
            .filter(|k| k.starts_with(&pre) && !k[pre.len()..].contains('/'))
            .map(|k| k[pre.len()..].to_string())
            .collect())
    }
    fn mkdir(&mut self, path: &str) -> Result<(), Said> {
        self.items.insert(path.to_string(), (None, 0));
        Ok(())
    }
    fn reflink(&mut self, from: &str, to: &str) -> Result<bool, Said> {
        if sv(from) != sv(to) {
            return Ok(false);
        }
        let v = self.items.get(from).cloned().ok_or("no source")?;
        self.items.insert(to.to_string(), v);
        self.reflinks += 1;
        Ok(true)
    }
    fn read(&mut self, path: &str, offset: u64, len: u64) -> Result<Vec<u8>, Said> {
        if self.fail_read.as_deref() == Some(path) {
            self.fail_read = None;
            return Err(String::from("the disk said no"));
        }
        let b = self.bytes(path).ok_or("not a file")?;
        let end = (offset + len).min(b.len() as u64) as usize;
        let out = b[offset as usize..end].to_vec();
        self.read_bytes += out.len() as u64;
        Ok(out)
    }
    fn write(&mut self, path: &str, offset: u64, bytes: &[u8]) -> Result<(), Said> {
        let e = self.items.entry(path.to_string()).or_insert((Some(Vec::new()), 0));
        let b = e.0.get_or_insert_with(Vec::new);
        if offset == 0 {
            b.clear();
        }
        b.extend_from_slice(bytes);
        if self.corrupt && !b.is_empty() {
            b[0] ^= 1;
        }
        Ok(())
    }
    fn set_modified(&mut self, path: &str, ns: u64) -> Result<(), Said> {
        self.items.get_mut(path).ok_or("gone")?.1 = ns;
        Ok(())
    }
    fn rename(&mut self, from: &str, to: &str) -> Result<bool, Said> {
        if sv(from) != sv(to) {
            return Ok(false);
        }
        let moved: Vec<String> =
            self.items.keys().filter(|k| *k == from || k.starts_with(&alloc::format!("{from}/"))).cloned().collect();
        for k in moved {
            let v = self.items.remove(&k).unwrap_or_default();
            self.items.insert(alloc::format!("{to}{}", &k[from.len()..]), v);
        }
        self.renames += 1;
        Ok(true)
    }
    fn remove(&mut self, path: &str) -> Result<(), Said> {
        self.items.remove(path).map(|_| ()).ok_or(String::from("not there"))
    }
}

fn run(job: &mut Job, t: &mut Mem) -> State {
    for _ in 0..1000 {
        let s = job.step(t, 4 << 20).clone();
        if s != State::Running {
            return s;
        }
    }
    panic!("never finished");
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

#[test]
fn a_tree_in_one_subvolume_is_reflinked() {
    let mut t = Mem::with(
        &[("/home/m/A/x.txt", b"x"), ("/home/m/A/B/y.txt", b"yy")],
        &["/home/m/A", "/home/m/A/B", "/home/m/C"],
    );
    let mut job = Job::new(&mut t, &s(&["/home/m/A"]), "/home/m/C", Mode::Copy, false).unwrap();
    assert_eq!((job.total_items, job.total_bytes), (4, 3));
    assert_eq!(run(&mut job, &mut t), State::Done);
    assert_eq!(t.bytes("/home/m/C/A/B/y.txt"), Some(&b"yy"[..]));
    assert_eq!((t.reflinks, job.reflinked, job.done_items, job.done_bytes), (2, 2, 4, 3));
    assert_eq!(t.items["/home/m/C/A"].1, 7, "the folder's time kept");
}

#[test]
fn across_subvolumes_through_the_buffer_a_step_at_a_time() {
    let big: Vec<u8> = (0..(2 * BUFFER + 500)).map(|i| (i % 251) as u8).collect();
    let mut t = Mem::with(&[("/system/big.bin", &big)], &[]);
    let mut job = Job::new(&mut t, &s(&["/system/big.bin"]), "/home/m", Mode::Copy, false).unwrap();
    assert_eq!(job.step(&mut t, BUFFER), &State::Running);
    assert_eq!(job.done_bytes, BUFFER, "one buffer a step at this budget");
    assert_eq!(run(&mut job, &mut t), State::Done);
    assert_eq!(t.bytes("/home/m/big.bin"), Some(&big[..]));
    assert_eq!(t.items["/home/m/big.bin"].1, 1234, "its modified time kept");
    assert_eq!(t.reflinks, 0);
}

#[test]
fn a_move_in_one_subvolume_is_a_rename_and_across_is_a_copy() {
    let mut t = Mem::with(&[("/home/m/A/x.txt", b"x"), ("/system/s.txt", b"sys")], &["/home/m/A", "/home/m/C"]);
    let mut job = Job::new(&mut t, &s(&["/home/m/A"]), "/home/m/C", Mode::Move, false).unwrap();
    assert_eq!(run(&mut job, &mut t), State::Done);
    assert_eq!((t.renames, t.bytes("/home/m/C/A/x.txt")), (1, Some(&b"x"[..])));
    assert!(!t.items.contains_key("/home/m/A"));
    let mut job = Job::new(&mut t, &s(&["/system/s.txt"]), "/home/m", Mode::Move, false).unwrap();
    assert_eq!(run(&mut job, &mut t), State::Done);
    assert!(t.bytes("/home/m/s.txt").is_some() && !t.items.contains_key("/system/s.txt"));
}

#[test]
fn conflicts_wait_for_a_choice() {
    let mut t = Mem::with(
        &[
            ("/home/m/a/x.txt", b"new"),
            ("/home/m/a/y.txt", b"new"),
            ("/home/m/a/z.txt", b"new"),
            ("/home/m/x.txt", b"old"),
            ("/home/m/y.txt", b"old"),
            ("/home/m/z.txt", b"old"),
        ],
        &["/home/m/a"],
    );
    let src = s(&["/home/m/a/x.txt", "/home/m/a/y.txt", "/home/m/a/z.txt"]);
    let mut job = Job::new(&mut t, &src, "/home/m", Mode::Copy, false).unwrap();
    let State::Conflict(c) = job.step(&mut t, 1 << 20).clone() else { panic!("no conflict") };
    assert_eq!((c.dst.as_str(), c.src_meta.size, c.dst_meta.size), ("/home/m/x.txt", 3, 3));
    job.decide(Choice::Skip, false);
    let State::Conflict(c) = job.step(&mut t, 1 << 20).clone() else { panic!("no second conflict") };
    assert_eq!(c.dst, "/home/m/y.txt");
    job.decide(Choice::KeepBoth, false);
    let State::Conflict(_) = job.step(&mut t, 1 << 20).clone() else { panic!("no third conflict") };
    job.decide(Choice::Replace, true);
    assert_eq!(run(&mut job, &mut t), State::Done);
    assert_eq!(t.bytes("/home/m/x.txt"), Some(&b"old"[..]), "skipped");
    assert_eq!((t.bytes("/home/m/y.txt"), t.bytes("/home/m/y (2).txt")), (Some(&b"old"[..]), Some(&b"new"[..])));
    assert_eq!(t.bytes("/home/m/z.txt"), Some(&b"new"[..]), "replaced");
    assert_eq!(job.done_items, job.total_items);
}

#[test]
fn let_me_decide_for_each_file() {
    let mut t = Mem::with(
        &[
            ("/home/m/a/x.txt", b"new"),
            ("/home/m/a/y.txt", b"new"),
            ("/home/m/x.txt", b"old"),
            ("/home/m/y.txt", b"old"),
        ],
        &["/home/m/a"],
    );
    let mut job = Job::new(&mut t, &s(&["/home/m/a/x.txt", "/home/m/a/y.txt"]), "/home/m", Mode::Copy, false).unwrap();
    assert!(matches!(job.step(&mut t, 1 << 20), State::Conflict(_)));
    let all = job.conflicts(&mut t).unwrap();
    assert_eq!(all.iter().map(|c| c.dst.as_str()).collect::<Vec<_>>(), ["/home/m/x.txt", "/home/m/y.txt"]);
    job.decide_each(&[
        (String::from("/home/m/a/x.txt"), Choice::Replace),
        (String::from("/home/m/a/y.txt"), Choice::KeepBoth),
    ]);
    assert_eq!(run(&mut job, &mut t), State::Done);
    assert_eq!(t.bytes("/home/m/x.txt"), Some(&b"new"[..]));
    assert_eq!((t.bytes("/home/m/y.txt"), t.bytes("/home/m/y (2).txt")), (Some(&b"old"[..]), Some(&b"new"[..])));
}

#[test]
fn a_failure_waits_for_try_again_or_skip() {
    let mut t = Mem::with(&[("/system/a.txt", b"aaa"), ("/system/b.txt", b"bbb")], &[]);
    t.fail_read = Some(String::from("/system/a.txt"));
    let mut job = Job::new(&mut t, &s(&["/system/a.txt", "/system/b.txt"]), "/home/m", Mode::Copy, false).unwrap();
    let State::Failed(path, words) = job.step(&mut t, 1 << 20).clone() else { panic!("no failure") };
    assert_eq!((path.as_str(), words.as_str()), ("/system/a.txt", "the disk said no"));
    job.retry(false, &mut t);
    assert_eq!(run(&mut job, &mut t), State::Done);
    assert_eq!(t.bytes("/home/m/a.txt"), Some(&b"aaa"[..]), "tried again");
    // Skip: the file it failed on isn't made.
    let mut t = Mem::with(&[("/system/a.txt", b"aaa"), ("/system/b.txt", b"bbb")], &[]);
    t.fail_read = Some(String::from("/system/a.txt"));
    let mut job = Job::new(&mut t, &s(&["/system/a.txt", "/system/b.txt"]), "/home/m", Mode::Copy, false).unwrap();
    job.step(&mut t, 1 << 20);
    job.retry(true, &mut t);
    assert_eq!(run(&mut job, &mut t), State::Done);
    assert!(t.bytes("/home/m/a.txt").is_none() && t.bytes("/home/m/b.txt").is_some());
    assert_eq!(job.done_items, job.total_items);
}

#[test]
fn cancel_leaves_no_half_file() {
    let big: Vec<u8> = vec![5; (3 * BUFFER) as usize];
    let mut t = Mem::with(&[("/system/big.bin", &big)], &[]);
    let mut job = Job::new(&mut t, &s(&["/system/big.bin"]), "/home/m", Mode::Copy, false).unwrap();
    job.step(&mut t, BUFFER);
    assert!(t.items.contains_key("/home/m/big.bin"), "part-way");
    job.cancel(&mut t);
    assert_eq!(job.state, State::Cancelled);
    assert!(!t.items.contains_key("/home/m/big.bin"));
}

/// A step keeps to its budget with verify too: the copy is read back a
/// buffer at a time, never all at once (a 64 MiB file read back in one
/// step outlasted the caller's wait, TASK-105A).
#[test]
fn a_verified_copy_reads_back_within_each_steps_budget() {
    let big: Vec<u8> = (0..3u32 << 20).map(|i| (i % 251) as u8).collect();
    let mut t = Mem::with(&[("/home/m/big.bin", &big)], &["/home/m/d"]);
    let mut job = Job::new(&mut t, &s(&["/home/m/big.bin"]), "/home/m/d", Mode::Copy, true).unwrap();
    let budget = 1 << 20;
    for _ in 0..100 {
        let before = t.read_bytes;
        let st = job.step(&mut t, budget).clone();
        let read = t.read_bytes - before;
        assert!(read <= budget + BUFFER, "a step read {read} bytes, over its budget");
        if st != State::Running {
            break;
        }
    }
    assert_eq!(job.state, State::Done);
    assert_eq!(t.read_bytes, 2 * big.len() as u64, "read once to copy, once to check");
    assert_eq!(t.bytes("/home/m/d/big.bin"), Some(&big[..]));
    assert_eq!(job.done_items, 1);
}

#[test]
fn verify_catches_a_copy_that_differs() {
    let mut t = Mem::with(&[("/home/m/a.txt", b"hello")], &["/home/m/d"]);
    t.corrupt = true;
    let mut job = Job::new(&mut t, &s(&["/home/m/a.txt"]), "/home/m/d", Mode::Copy, true).unwrap();
    let State::Failed(_, words) = run(&mut job, &mut t) else { panic!("verify missed it") };
    assert_eq!(words, "The copy of a.txt doesn't match it.");
    assert_eq!(t.reflinks, 0, "verify never reflinks");
    t.corrupt = false;
    job.retry(false, &mut t);
    assert_eq!(run(&mut job, &mut t), State::Done);
    assert_eq!(t.bytes("/home/m/d/a.txt"), Some(&b"hello"[..]));
}

#[test]
fn into_its_own_folder_a_copy_gets_a_free_name_and_never_inside_itself() {
    let mut t = Mem::with(&[("/home/m/notes.txt", b"n")], &["/home/m/A"]);
    let mut job = Job::new(&mut t, &s(&["/home/m/notes.txt"]), "/home/m", Mode::Copy, false).unwrap();
    assert_eq!(run(&mut job, &mut t), State::Done);
    assert_eq!(t.bytes("/home/m/notes (2).txt"), Some(&b"n"[..]));
    assert!(Job::new(&mut t, &s(&["/home/m/A"]), "/home/m/A", Mode::Copy, false).is_err());
    assert!(Job::new(&mut t, &s(&["/home/m"]), "/home/m/A", Mode::Copy, false).is_err());
    let mut job = Job::new(&mut t, &s(&["/home/m/A"]), "/home/m", Mode::Copy, false).unwrap();
    assert_eq!(run(&mut job, &mut t), State::Done);
    assert!(t.items.contains_key("/home/m/A (2)"));
}

#[test]
fn many_small_files_go_in_one_step() {
    let files: Vec<(String, Vec<u8>)> =
        (0..100).map(|i| (alloc::format!("/system/f{i:03}.txt"), vec![1u8; 10])).collect();
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
    let mut t = Mem::with(&refs, &[]);
    let src: Vec<String> = files.iter().map(|(p, _)| p.clone()).collect();
    let mut job = Job::new(&mut t, &src, "/home/m", Mode::Copy, false).unwrap();
    job.step(&mut t, 4 << 20);
    assert_eq!(job.done_items, 100, "100 small files in one step");
}
