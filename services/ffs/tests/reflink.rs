// SPDX-License-Identifier: Apache-2.0
//! Reflinks (TASK-105A, FR-085): a copy in one subvolume shares every
//! block of its source. Nothing new is written but the new file's records;
//! the copy reads the same; a write to either takes new blocks for itself
//! and leaves the other as it was; removing either keeps the other whole;
//! removing both frees the blocks; `ffsck` is clean at every step, sealed
//! homes included.

use svc_ffs::FsError;
use svc_ffs::inode::{FileKind, ROOT_INO};
use svc_ffs::store::MemStore;
use svc_ffs::subvol::SYSTEM;
use svc_ffs::volume::Volume;

fn content() -> Vec<u8> {
    (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect()
}

fn clean(v: &mut Volume<MemStore>) {
    let r = v.fsck().unwrap();
    assert!(r.is_clean(), "{:?}", r.findings);
}

fn read(v: &mut Volume<MemStore>, sv: u64, ino: u64) -> Vec<u8> {
    let n = v.stat(sv, ino).unwrap().size;
    v.read(sv, ino, 0, n).unwrap()
}

fn shares_and_stays_apart(mut v: Volume<MemStore>, sv: u64) {
    let a = v.create(sv, ROOT_INO, "a.bin", FileKind::File).unwrap();
    v.write(sv, a, 0, &content()).unwrap();
    // Uncommitted: the clone commits first (a block still changing in place can't be shared).
    let free = v.free_blocks();
    let b = v.clone_file(sv, a, ROOT_INO, "b.bin").unwrap();
    v.commit().unwrap();
    let used = free.saturating_sub(v.free_blocks());
    assert!(used < 8, "a reflink of {} bytes took {used} new blocks", content().len());
    assert_eq!(read(&mut v, sv, b), content());
    assert_eq!(v.stat(sv, b).unwrap().size, content().len() as u64);
    clean(&mut v);
    // A write to the copy: the source as it was.
    v.write(sv, b, 4096 * 3 + 5, b"changed in the copy").unwrap();
    v.commit().unwrap();
    assert_eq!(read(&mut v, sv, a), content());
    let mut want = content();
    want[4096 * 3 + 5..4096 * 3 + 5 + 19].copy_from_slice(b"changed in the copy");
    assert_eq!(read(&mut v, sv, b), want);
    clean(&mut v);
    // The source removed: the copy whole.
    v.unlink(sv, ROOT_INO, "a.bin").unwrap();
    v.commit().unwrap();
    assert_eq!(read(&mut v, sv, b), want);
    clean(&mut v);
    // Both gone: their blocks free again.
    v.unlink(sv, ROOT_INO, "b.bin").unwrap();
    v.commit().unwrap();
    clean(&mut v);
    assert!(v.free_blocks() + 8 >= free, "blocks still used after both went: {} of {free}", v.free_blocks());
    // A directory, or another subvolume, isn't a reflink.
    let d = v.create(sv, ROOT_INO, "d", FileKind::Dir).unwrap();
    assert_eq!(v.clone_file(sv, d, ROOT_INO, "e"), Err(FsError::IsADirectory));
}

#[test]
fn a_reflink_in_a_plain_subvolume() {
    let v = Volume::format(MemStore::new(4096), [1; 16]).unwrap();
    shares_and_stays_apart(v, SYSTEM);
}

#[test]
fn a_reflink_in_a_sealed_home() {
    let mut v = Volume::format(MemStore::new(4096), [1; 16]).unwrap();
    let home = v.create_encrypted_subvol("home:mike", [7; 16], &[0x4B; 32]).unwrap();
    shares_and_stays_apart(v, home);
}

#[test]
fn a_reflink_survives_a_mount() {
    let mut v = Volume::format(MemStore::new(4096), [1; 16]).unwrap();
    let a = v.create(SYSTEM, ROOT_INO, "a.bin", FileKind::File).unwrap();
    v.write(SYSTEM, a, 0, &content()).unwrap();
    let b = v.clone_file(SYSTEM, a, ROOT_INO, "b.bin").unwrap();
    v.commit().unwrap();
    let mut v = Volume::mount(v.into_store()).unwrap();
    assert_eq!(read(&mut v, SYSTEM, b), content());
    clean(&mut v);
}
