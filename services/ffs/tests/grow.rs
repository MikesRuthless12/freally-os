// SPDX-License-Identifier: Apache-2.0
//! A volume grown with its partition (TASK-114: a tester's virtual disk made
//! bigger before the first boot): the new blocks are free and used, the
//! files there before read the same, ffsck is clean, and a remount sees the
//! new size.

use svc_ffs::FsError;
use svc_ffs::inode::{FileKind, ROOT_INO};
use svc_ffs::store::MemStore;
use svc_ffs::subvol::SYSTEM;
use svc_ffs::volume::Volume;

#[test]
fn a_grown_partition_grows_the_volume() {
    let mut v = Volume::format(MemStore::new(512), [5; 16]).unwrap();
    let kept = v.create(SYSTEM, ROOT_INO, "kept.txt", FileKind::File).unwrap();
    v.write(SYSTEM, kept, 0, b"here before").unwrap();
    v.commit().unwrap();
    // More than the old volume holds: refused.
    let big: Vec<u8> = (0..3u32 << 20).map(|i| (i % 249) as u8).collect();
    let f = v.create(SYSTEM, ROOT_INO, "big.bin", FileKind::File).unwrap();
    assert_eq!(v.write(SYSTEM, f, 0, &big).and_then(|_| v.commit()), Err(FsError::NoSpace));
    let mut v = Volume::mount_checked(v.into_store()).unwrap().0;
    // The disk grows; the volume follows, once.
    v.store_mut().grow(2048);
    assert_eq!(v.grow(2048), Ok(true));
    assert_eq!(v.grow(2048), Ok(false), "nothing more to grow into");
    assert_eq!(v.grow(4096), Ok(false), "not past the disk");
    assert_eq!(v.blocks(), 2048);
    let f = match v.lookup(SYSTEM, ROOT_INO, "big.bin").unwrap() {
        Some(e) => e.child,
        None => v.create(SYSTEM, ROOT_INO, "big.bin", FileKind::File).unwrap(),
    };
    v.write(SYSTEM, f, 0, &big).unwrap();
    v.commit().unwrap();
    assert!(v.fsck().unwrap().is_clean());
    let mut v = Volume::mount(v.into_store()).unwrap();
    assert_eq!(v.blocks(), 2048);
    assert_eq!(v.read(SYSTEM, f, 0, big.len() as u64).unwrap(), big);
    assert_eq!(v.read(SYSTEM, kept, 0, 64).unwrap(), b"here before");
    assert!(v.fsck().unwrap().is_clean());
}
