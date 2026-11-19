// SPDX-License-Identifier: Apache-2.0
//! FreallyFS part 3 (TASK-096): subvolumes and snapshots. A snapshot keeps
//! exactly what its source held while the source goes on changing (files
//! written over, truncated, unlinked, made); every commit passes `ffsck`
//! (each block's pointers equal its reference count, nothing leaked);
//! deleting the source keeps the snapshot whole, deleting everything gives
//! every block back; a snapshot is read-only; a crash with blocks shared
//! still mounts one whole generation.

use std::collections::BTreeMap;
use svc_ffs::FsError;
use svc_ffs::fsck::Report;
use svc_ffs::inode::{FileKind, ROOT_INO};
use svc_ffs::store::{MemStore, Store};
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

type Tree = BTreeMap<String, Option<Vec<u8>>>;

fn dump<S: Store>(v: &mut Volume<S>, sv: u64) -> Tree {
    let mut out = Tree::new();
    let mut todo = vec![(ROOT_INO, String::new())];
    while let Some((dir, path)) = todo.pop() {
        for e in v.read_dir(sv, dir).unwrap() {
            let p = format!("{path}/{}", e.name);
            if e.kind == FileKind::Dir {
                out.insert(p.clone(), None);
                todo.push((e.child, p));
            } else {
                let size = v.stat(sv, e.child).unwrap().size;
                out.insert(p, Some(v.read(sv, e.child, 0, size).unwrap()));
            }
        }
    }
    out
}

fn clean(r: &Report, when: &str) {
    assert!(r.is_clean(), "{when}: {:#?}", r.findings);
}

/// Random changes to subvolume `sv`'s files (flat: the root directory).
fn churn<S: Store>(v: &mut Volume<S>, sv: u64, m: &mut Tree, r: &mut Rng, steps: u32) {
    for _ in 0..steps {
        let name = format!("f{}", r.below(40));
        let path = format!("/{name}");
        let ino = v.lookup(sv, ROOT_INO, &name).unwrap().map(|e| e.child);
        match (r.below(10), ino) {
            (0..2, None) => {
                v.create(sv, ROOT_INO, &name, FileKind::File).unwrap();
                m.insert(path, Some(Vec::new()));
            }
            (2..7, Some(ino)) => {
                let off = r.below(30_000);
                let len = r.below(12_000) as usize;
                let seed = r.next() as u8;
                let data: Vec<u8> = (0..len).map(|i| seed.wrapping_add(i as u8)).collect();
                v.write(sv, ino, off, &data).unwrap();
                let f = m.get_mut(&path).unwrap().as_mut().unwrap();
                if f.len() < off as usize + len {
                    f.resize(off as usize + len, 0);
                }
                f[off as usize..off as usize + len].copy_from_slice(&data);
            }
            (7..9, Some(ino)) => {
                let size = r.below(20_000);
                v.truncate(sv, ino, size).unwrap();
                m.get_mut(&path).unwrap().as_mut().unwrap().resize(size as usize, 0);
            }
            (9, Some(_)) => {
                v.unlink(sv, ROOT_INO, &name).unwrap();
                m.remove(&path);
            }
            _ => {}
        }
    }
}

#[test]
fn a_snapshot_keeps_what_its_source_held() {
    let mut v = Volume::format(MemStore::new(16_384), [8; 16]).unwrap();
    let free_at_start = v.free_blocks();
    let home = v.create_subvol("home of a tester", [1; 16]).unwrap();
    let mut live = Tree::new();
    let mut r = Rng(0xD00D);
    churn(&mut v, home, &mut live, &mut r, 200);
    let mut snaps: Vec<(u64, Tree)> = Vec::new();
    for round in 0..6 {
        let id = v.snapshot(home, &format!("snapshot {round}")).unwrap();
        snaps.push((id, live.clone()));
        churn(&mut v, home, &mut live, &mut r, 150);
        v.commit().unwrap();
        clean(&v.fsck().unwrap(), &format!("round {round}"));
        assert_eq!(dump(&mut v, home), live, "round {round}: the source");
        for (id, frozen) in &snaps {
            assert_eq!(&dump(&mut v, *id), frozen, "round {round}: snapshot {id}");
        }
    }
    // Remounted, all the same.
    let mut v = Volume::mount(v.into_store()).unwrap();
    clean(&v.fsck().unwrap(), "remounted");
    for (id, frozen) in &snaps {
        assert_eq!(&dump(&mut v, *id), frozen);
    }
    // A snapshot can't change.
    let (sid, _) = &snaps[0];
    assert_eq!(v.create(*sid, ROOT_INO, "new", FileKind::File), Err(FsError::ReadOnly));
    let any = v.read_dir(*sid, ROOT_INO).unwrap().into_iter().find(|e| e.kind == FileKind::File).unwrap();
    assert_eq!(v.write(*sid, any.child, 0, b"x"), Err(FsError::ReadOnly));
    // The source goes; the snapshots stay whole.
    v.delete_subvol(home).unwrap();
    v.commit().unwrap();
    clean(&v.fsck().unwrap(), "the source deleted");
    for (id, frozen) in &snaps {
        assert_eq!(&dump(&mut v, *id), frozen, "snapshot {id} after its source went");
    }
    assert_eq!(v.subvols().unwrap().len(), 1 + snaps.len());
    // Every snapshot goes: every block comes back.
    for (id, _) in &snaps {
        v.delete_subvol(*id).unwrap();
        v.commit().unwrap();
        clean(&v.fsck().unwrap(), &format!("snapshot {id} deleted"));
    }
    v.commit().unwrap();
    v.commit().unwrap();
    assert!(v.free_blocks() + 8 >= free_at_start, "{} free, {free_at_start} at the start", v.free_blocks());
    assert_eq!(v.delete_subvol(svc_ffs::subvol::SYSTEM), Err(FsError::ReadOnly), "the system's stays");
}

#[test]
fn a_crash_with_blocks_shared_mounts_one_whole_generation() {
    let base = || {
        let mut v = Volume::format(MemStore::new(4096), [9; 16]).unwrap();
        let home = v.create_subvol("home", [2; 16]).unwrap();
        let mut m = Tree::new();
        let mut r = Rng(5);
        churn(&mut v, home, &mut m, &mut r, 120);
        let snap = v.snapshot(home, "before").unwrap();
        let frozen = m.clone();
        v.commit().unwrap();
        let before = m.clone();
        churn(&mut v, home, &mut m, &mut r, 80);
        (v, home, snap, frozen, before, m)
    };
    let (mut v, ..) = base();
    let w0 = v.store_mut().writes;
    v.commit().unwrap();
    let writes = v.store_mut().writes - w0;
    for (n, mode) in (0..=writes + 1).flat_map(|n| (0..4).map(move |m| (n, m))) {
        let (mut v, home, snap, frozen, before, after) = base();
        let at = v.store_mut().writes + n;
        v.store_mut().fail_after = Some(at);
        let _ = v.commit();
        let mut store = v.into_store();
        store.crash(|b| match mode {
            0 => false,
            1 => b % 3 == 0,
            2 => true,
            _ => b < 2,
        });
        let mut m = Volume::mount(store).unwrap();
        clean(&m.fsck().unwrap(), &format!("crash at step {n}, mode {mode}"));
        let got = dump(&mut m, home);
        assert!(got == before || got == after, "crash at step {n}, mode {mode}: neither generation");
        assert_eq!(dump(&mut m, snap), frozen, "crash at step {n}, mode {mode}: the snapshot");
    }
}
