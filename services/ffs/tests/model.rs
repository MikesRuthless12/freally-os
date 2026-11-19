// SPDX-License-Identifier: Apache-2.0
//! FreallyFS part 1 against a model (TASK-094): random inserts, deletes and
//! reads on a file-backed disk, checked against a `BTreeMap` after every
//! commit and every remount, the volume's structure checked each time
//! (every node read and verified, no block reached twice, none leaked);
//! and a crash at every write of a commit leaving the last committed
//! generation whole.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use svc_ffs::store::{MemStore, Store};
use svc_ffs::volume::Volume;
use svc_ffs::{BLOCK, FsError, Key};

/// A volume in a file (on the build disk, not a RAM disk).
struct FileStore {
    file: File,
    blocks: u64,
}

impl FileStore {
    fn create(name: &str, blocks: u64) -> FileStore {
        let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        let path = dir.join(name);
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(path).unwrap();
        file.set_len(blocks * BLOCK as u64).unwrap();
        FileStore { file, blocks }
    }
}

impl Store for FileStore {
    fn blocks(&self) -> u64 {
        self.blocks
    }
    fn read(&mut self, block: u64, buf: &mut [u8; BLOCK]) -> Result<(), FsError> {
        self.file.seek(SeekFrom::Start(block * BLOCK as u64)).map_err(|_| FsError::Io)?;
        self.file.read_exact(buf).map_err(|_| FsError::Io)
    }
    fn write(&mut self, block: u64, buf: &[u8; BLOCK]) -> Result<(), FsError> {
        self.file.seek(SeekFrom::Start(block * BLOCK as u64)).map_err(|_| FsError::Io)?;
        self.file.write_all(buf).map_err(|_| FsError::Io)
    }
    fn flush(&mut self) -> Result<(), FsError> {
        self.file.sync_data().map_err(|_| FsError::Io)
    }
}

/// xorshift64*: the same run every time.
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

fn value(r: &mut Rng) -> Vec<u8> {
    let len = match r.below(100) {
        0..70 => r.below(64),
        70..95 => 64 + r.below(536),
        _ => 600 + r.below(2401),
    } as usize;
    let seed = r.next() as u8;
    (0..len).map(|i| seed.wrapping_add(i as u8)).collect()
}

fn same<S: Store>(v: &mut Volume<S>, model: &BTreeMap<Key, Vec<u8>>, when: &str) {
    let items = v.items().unwrap();
    assert_eq!(items.len(), model.len(), "{when}: the item count");
    for ((k, val), (mk, mv)) in items.iter().zip(model) {
        assert_eq!((k, val), (mk, mv), "{when}");
    }
}

#[test]
fn random_inserts_and_deletes_match_a_model() {
    let blocks = 8192; // 32 MiB
    let mut v = Volume::format(FileStore::create("model.ffs", blocks), [7; 16]).unwrap();
    let mut model: BTreeMap<Key, Vec<u8>> = BTreeMap::new();
    let mut r = Rng(0x00C0_FFEE_D00D_F00D);
    let (mut commits, mut remounts) = (0, 0);
    for step in 0..6000u32 {
        match r.below(100) {
            0..60 => {
                let k = Key::new(r.below(900), 1, r.below(4));
                let val = value(&mut r);
                v.insert(k, &val).unwrap();
                model.insert(k, val);
            }
            60..90 => {
                let k = if !model.is_empty() && r.below(5) != 0 {
                    *model.keys().nth(r.below(model.len() as u64) as usize).unwrap()
                } else {
                    Key::new(r.below(900), 1, r.below(4))
                };
                assert_eq!(v.delete(&k).unwrap(), model.remove(&k), "step {step}: delete {k:?}");
            }
            _ => {
                let k = Key::new(r.below(900), 1, r.below(4));
                assert_eq!(v.get(&k).unwrap().as_ref(), model.get(&k), "step {step}: get {k:?}");
            }
        }
        if step % 97 == 96 {
            v.commit().unwrap();
            commits += 1;
            let report = v.check().unwrap_or_else(|e| panic!("step {step}: the check: {e:?}"));
            assert!(report.nodes >= 3);
            if commits % 7 == 0 {
                let store = v.into_store();
                v = Volume::mount(store).unwrap();
                remounts += 1;
                same(&mut v, &model, &format!("after remount {remounts}"));
            }
        }
    }
    v.commit().unwrap();
    v.check().unwrap();
    same(&mut v, &model, "at the end");
    // Delete everything: the tree empties back to one leaf, nothing leaks.
    for k in model.keys().copied().collect::<Vec<_>>() {
        assert!(v.delete(&k).unwrap().is_some());
    }
    v.commit().unwrap();
    let r = v.check().unwrap();
    let v2 = Volume::mount(v.into_store()).unwrap();
    assert_eq!(r.nodes, 3 + (v2.superblock().used_blocks - 3), "only the trees' nodes remain");
    assert!(commits > 50 && remounts > 5, "{commits} commits, {remounts} remounts");
}

#[test]
fn a_crash_at_any_write_of_a_commit_leaves_the_last_generation() {
    // A volume with some history, committed: the state to come back to.
    let base = || {
        let mut v = Volume::format(MemStore::new(2048), [1; 16]).unwrap();
        let mut r = Rng(42);
        let mut model = BTreeMap::new();
        for _ in 0..300 {
            let k = Key::new(r.below(300), 1, 0);
            let val = value(&mut r);
            v.insert(k, &val).unwrap();
            model.insert(k, val);
        }
        v.commit().unwrap();
        // The next generation's changes, not yet committed.
        let mut next = model.clone();
        for _ in 0..200 {
            let k = Key::new(r.below(300), 1, 0);
            if r.below(3) == 0 {
                v.delete(&k).unwrap();
                next.remove(&k);
            } else {
                let val = value(&mut r);
                v.insert(k, &val).unwrap();
                next.insert(k, val);
            }
        }
        (v, model, next)
    };
    // How many writes a whole commit makes.
    let (mut v, _, _) = base();
    let before = v.store_mut().writes;
    v.commit().unwrap();
    let writes = v.store_mut().writes - before;
    assert!(writes > 10, "{writes} writes");
    // A crash before each write, and before the last flush (n = writes);
    // the disk keeps none of its cache, some, all of it, or only the
    // superblocks (written from its cache before the nodes).
    for (n, mode) in (0..=writes + 1).flat_map(|n| (0..4).map(move |m| (n, m))) {
        let (mut v, model, next) = base();
        let at = v.store_mut().writes + n;
        v.store_mut().fail_after = Some(at);
        let failed = v.commit().is_err();
        assert_eq!(failed, n <= writes, "the commit stopped at step {n}");
        let mut store = v.into_store();
        store.crash(|b| match mode {
            0 => false,
            1 => b % 3 == 0,
            2 => true,
            _ => b < 2,
        });
        let mut m = Volume::mount(store).unwrap();
        // Never a mix: the generation before, whole, or (when the disk kept
        // the new superblock after the nodes' flush) the new one, whole.
        match m.generation() {
            2 => same(&mut m, &model, &format!("crash at step {n}")),
            3 => {
                assert!(n >= writes - 1 || !failed, "crash at step {n}: a superblock before its nodes were flushed");
                same(&mut m, &next, &format!("crash at step {n}"));
            }
            g => panic!("crash at step {n}: generation {g}"),
        }
        m.check().unwrap_or_else(|e| panic!("crash at write {n}: {e:?}"));
    }
}

#[test]
fn a_damaged_node_is_caught_never_trusted() {
    let mut v = Volume::format(MemStore::new(1024), [2; 16]).unwrap();
    for i in 0..400u64 {
        v.insert(Key::new(i, 1, 0), &[i as u8; 40]).unwrap();
    }
    v.commit().unwrap();
    let leaf = {
        let sb = *v.superblock();
        sb.root_tree.block
    };
    let mut store = v.into_store();
    store.flip(leaf, 200);
    let err = Volume::mount(store).and_then(|mut m| m.items().map(|_| ()));
    assert_eq!(err, Err(FsError::Integrity(leaf)));
}
