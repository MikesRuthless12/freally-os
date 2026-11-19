// SPDX-License-Identifier: Apache-2.0
//! FreallyFS part 2 (TASK-095): files and directories against a model:
//! creates, sparse and overlapping writes, reads, truncates and unlinks,
//! with commits (each checked: every node and data block read and
//! verified), remounts and the clean flag; and a crash at every write step
//! of a commit holding file changes always mounting one whole generation.

use std::collections::BTreeMap;
use svc_ffs::FsError;
use svc_ffs::inode::{FileKind, ROOT_INO};
use svc_ffs::store::{MemStore, Store};
use svc_ffs::subvol::SYSTEM;
use svc_ffs::volume::Volume;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Every path under the root: a directory (None) or a file's bytes.
type Tree = BTreeMap<String, Option<Vec<u8>>>;

fn dump<S: Store>(v: &mut Volume<S>) -> Tree {
    let mut out = Tree::new();
    let mut todo = vec![(ROOT_INO, String::new())];
    while let Some((dir, path)) = todo.pop() {
        for e in v.read_dir(SYSTEM, dir).unwrap() {
            let p = format!("{path}/{}", e.name);
            match e.kind {
                FileKind::Dir => {
                    out.insert(p.clone(), None);
                    todo.push((e.child, p));
                }
                FileKind::File => {
                    let size = v.stat(SYSTEM, e.child).unwrap().size;
                    out.insert(p, Some(v.read(SYSTEM, e.child, 0, size).unwrap()));
                }
            }
        }
    }
    out
}

fn bytes(r: &mut Rng, n: usize) -> Vec<u8> {
    let seed = r.next() as u8;
    (0..n).map(|i| seed.wrapping_add((i * 13) as u8)).collect()
}

/// One random step on `v` and the model `m` (directories `dirs`, files
/// `files`: their inodes and paths).
fn step<S: Store>(
    v: &mut Volume<S>,
    m: &mut Tree,
    dirs: &mut Vec<(u64, String)>,
    files: &mut Vec<(u64, u64, String)>,
    r: &mut Rng,
) {
    match r.below(100) {
        0..12 => {
            let (d, path) = dirs[r.below(dirs.len() as u64) as usize].clone();
            let name = format!("f{}", r.below(10_000));
            let p = format!("{path}/{name}");
            match v.create(SYSTEM, d, &name, FileKind::File) {
                Ok(ino) => {
                    m.insert(p.clone(), Some(Vec::new()));
                    files.push((ino, d, p));
                }
                Err(FsError::Exists) => assert!(m.contains_key(&p)),
                Err(e) => panic!("create {p}: {e:?}"),
            }
        }
        12..16 => {
            let (d, path) = dirs[r.below(dirs.len() as u64) as usize].clone();
            let name = format!("d{}", r.below(1000));
            let p = format!("{path}/{name}");
            if let Ok(ino) = v.create(SYSTEM, d, &name, FileKind::Dir) {
                m.insert(p.clone(), None);
                dirs.push((ino, p));
            }
        }
        16..60 if !files.is_empty() => {
            let (ino, _, p) = files[r.below(files.len() as u64) as usize].clone();
            // Mostly near the start; sometimes far out (a hole between).
            let off = if r.below(8) == 0 { r.below(300_000) } else { r.below(20_000) };
            let len = if r.below(10) == 0 {
                4096 * (1 + r.below(70)) as usize + r.below(4096) as usize
            } else {
                r.below(9000) as usize
            };
            let data = bytes(r, len);
            v.write(SYSTEM, ino, off, &data).unwrap();
            let f = m.get_mut(&p).unwrap().as_mut().unwrap();
            if f.len() < off as usize + len {
                f.resize(off as usize + len, 0);
            }
            f[off as usize..off as usize + len].copy_from_slice(&data);
        }
        60..75 if !files.is_empty() => {
            let (ino, _, p) = files[r.below(files.len() as u64) as usize].clone();
            let f = m[&p].as_ref().unwrap().clone();
            let off = r.below(f.len() as u64 + 10);
            let len = r.below(30_000);
            let want: Vec<u8> = f.iter().skip(off as usize).take(len as usize).copied().collect();
            assert_eq!(v.read(SYSTEM, ino, off, len).unwrap(), want, "read {p} at {off}+{len}");
        }
        75..85 if !files.is_empty() => {
            let (ino, _, p) = files[r.below(files.len() as u64) as usize].clone();
            let size = r.below(40_000);
            v.truncate(SYSTEM, ino, size).unwrap();
            m.get_mut(&p).unwrap().as_mut().unwrap().resize(size as usize, 0);
        }
        85..92 if !files.is_empty() => {
            let i = r.below(files.len() as u64) as usize;
            let (_, d, p) = files.swap_remove(i);
            let name = p.rsplit('/').next().unwrap();
            v.unlink(SYSTEM, d, name).unwrap();
            m.remove(&p);
        }
        _ => {}
    }
}

#[test]
fn files_and_directories_match_a_model() {
    let mut v = Volume::format(MemStore::new(16_384), [5; 16]).unwrap();
    v.set_time(1_763_500_000_000_000_000);
    let mut m = Tree::new();
    m.insert(String::from("/accounts"), None);
    let accounts = v.lookup(SYSTEM, ROOT_INO, "accounts").unwrap().unwrap().child;
    let mut dirs = vec![(ROOT_INO, String::new()), (accounts, String::from("/accounts"))];
    let mut files: Vec<(u64, u64, String)> = Vec::new();
    let mut r = Rng(0x5EED_F11E);
    assert_eq!(dump(&mut v), m, "a new volume: the root and accounts");
    for i in 0..3000u32 {
        step(&mut v, &mut m, &mut dirs, &mut files, &mut r);
        if i % 150 == 149 {
            v.commit().unwrap();
            let report = v.fsck().unwrap();
            assert!(report.is_clean(), "step {i}: {:?}", report.findings);
            if i % 600 == 599 {
                let store = v.unmount().unwrap();
                v = Volume::mount(store).unwrap();
                assert!(v.was_clean(), "unmounted: clean");
            }
            assert_eq!(dump(&mut v), m, "after step {i}");
        }
    }
    assert!(files.len() > 10, "{} files", files.len());
    // A file and its blocks go with its last link; a directory only empty.
    let (ino, d, p) = files[0].clone();
    let name = p.rsplit('/').next().unwrap().to_string();
    v.unlink(SYSTEM, d, &name).unwrap();
    assert_eq!(v.stat(SYSTEM, ino), Err(FsError::NotFound));
    v.create(SYSTEM, accounts, "keep", FileKind::File).ok();
    assert_eq!(v.unlink(SYSTEM, ROOT_INO, "accounts"), Err(FsError::NotEmpty));
    assert_eq!(v.unlink(SYSTEM, ROOT_INO, "missing"), Err(FsError::NotFound));
    assert_eq!(v.create(SYSTEM, ROOT_INO, "a/b", FileKind::File), Err(FsError::BadName));
    assert_eq!(v.write(SYSTEM, accounts, 0, b"x"), Err(FsError::IsADirectory));
    v.commit().unwrap();
    let report = v.fsck().unwrap();
    assert!(report.is_clean(), "{:?}", report.findings);
    // Not unmounted: found not clean, and checked as it mounts.
    let (v, report) = Volume::mount_checked(v.into_store()).unwrap();
    assert!(!v.was_clean());
    let report = report.expect("ffsck runs at an unclean mount");
    assert!(report.is_clean(), "{:?}", report.findings);
    // Unmounted: no check needed.
    let (_, report) = Volume::mount_checked(v.unmount().unwrap()).unwrap();
    assert!(report.is_none());
}

#[test]
fn a_crash_at_any_write_of_a_file_commit_leaves_one_whole_generation() {
    let base = || {
        let mut v = Volume::format(MemStore::new(4096), [6; 16]).unwrap();
        let mut m = Tree::new();
        m.insert(String::from("/accounts"), None);
        let mut dirs = vec![(ROOT_INO, String::new())];
        let mut files = Vec::new();
        let mut r = Rng(77);
        for _ in 0..120 {
            step(&mut v, &mut m, &mut dirs, &mut files, &mut r);
        }
        v.commit().unwrap();
        let before = m.clone();
        for _ in 0..60 {
            step(&mut v, &mut m, &mut dirs, &mut files, &mut r);
        }
        (v, before, m)
    };
    let (mut v, _, _) = base();
    let w0 = v.store_mut().writes;
    v.commit().unwrap();
    let writes = v.store_mut().writes - w0;
    assert!(writes > 20, "{writes} writes (data and nodes)");
    for (n, mode) in (0..=writes + 1).flat_map(|n| (0..4).map(move |m| (n, m))) {
        let (mut v, before, after) = base();
        let at = v.store_mut().writes + n;
        v.store_mut().fail_after = Some(at);
        let failed = v.commit().is_err();
        assert_eq!(failed, n <= writes);
        let mut store = v.into_store();
        store.crash(|b| match mode {
            0 => false,
            1 => b % 3 == 0,
            2 => true,
            _ => b < 2,
        });
        let mut m = Volume::mount(store).unwrap();
        let got = dump(&mut m);
        match m.generation() {
            2 => assert_eq!(got, before, "crash at step {n}, mode {mode}"),
            3 => assert_eq!(got, after, "crash at step {n}, mode {mode}"),
            g => panic!("generation {g}"),
        }
        let report = m.fsck().unwrap();
        assert!(report.is_clean(), "crash at step {n}, mode {mode}: {:?}", report.findings);
    }
}

#[test]
fn rename_moves_files_and_directories_and_keeps_the_volume_whole() {
    let mut v = Volume::format(MemStore::new(4096), [3; 16]).unwrap();
    let a = v.create(SYSTEM, ROOT_INO, "a", FileKind::Dir).unwrap();
    let b = v.create(SYSTEM, a, "b", FileKind::Dir).unwrap();
    let f = v.create(SYSTEM, a, "note.txt", FileKind::File).unwrap();
    v.write(SYSTEM, f, 0, b"hello").unwrap();
    // A file within its directory, then across.
    v.rename(SYSTEM, a, "note.txt", a, "renamed.txt").unwrap();
    assert!(v.lookup(SYSTEM, a, "note.txt").unwrap().is_none());
    v.rename(SYSTEM, a, "renamed.txt", b, "moved.txt").unwrap();
    assert_eq!(v.lookup(SYSTEM, b, "moved.txt").unwrap().unwrap().child, f);
    assert_eq!(v.read(SYSTEM, f, 0, 5).unwrap(), b"hello");
    // A directory up a level: its parent and the links follow.
    v.rename(SYSTEM, a, "b", ROOT_INO, "b").unwrap();
    assert_eq!(v.stat(SYSTEM, b).unwrap().parent, ROOT_INO);
    // Not into itself, not over a name, not from nowhere.
    let c = v.create(SYSTEM, b, "c", FileKind::Dir).unwrap();
    assert_eq!(v.rename(SYSTEM, ROOT_INO, "b", c, "loop"), Err(FsError::BadName));
    assert_eq!(v.rename(SYSTEM, ROOT_INO, "b", ROOT_INO, "a"), Err(FsError::Exists));
    assert_eq!(v.rename(SYSTEM, ROOT_INO, "missing", ROOT_INO, "x"), Err(FsError::NotFound));
    v.commit().unwrap();
    let r = v.fsck().unwrap();
    assert!(r.is_clean(), "{:?}", r.findings);
}
