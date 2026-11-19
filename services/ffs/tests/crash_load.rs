// SPDX-License-Identifier: Apache-2.0
//! The crash test's load (TASK-099) on the host: a crash at every write of a
//! transaction, losing the unflushed writes four ways; every mount after it
//! holds one whole transaction (the one before or the one being written),
//! and ffsck finds nothing. `cargo xtask crash-test` runs the same load in
//! the OS under real resets.

use svc_ffs::load::{FILES, content};
use svc_ffs::store::MemStore;
use svc_ffs::volume::Volume;

#[test]
fn content_names_its_file_and_version() {
    let a = content(1, 7);
    assert_eq!(&a[..8], b"FRLYLOAD");
    assert_eq!(a[8..16], 1u64.to_le_bytes());
    assert_eq!(a[16..24], 7u64.to_le_bytes());
    assert!((1024..=41 * 1024).contains(&a.len()));
    assert_eq!(a, content(1, 7), "the same bytes every time");
    assert_ne!(a, content(1, 8));
    assert_ne!(a, content(FILES, 7));
}

#[test]
fn the_load_is_whole_after_a_crash_at_any_write() {
    let fresh = || {
        let mut v = Volume::format(MemStore::new(4096), [9; 16]).unwrap();
        let mut l = v.load_open().unwrap();
        assert_eq!(l.version, 0);
        for _ in 0..3 {
            v.load_step(&mut l).unwrap();
        }
        (v, l)
    };
    let (mut v, mut l) = fresh();
    let w0 = v.store_mut().writes;
    v.load_step(&mut l).unwrap();
    let writes = v.store_mut().writes - w0;
    assert!(writes > 20, "{writes} writes (data and nodes)");
    for (n, mode) in (0..=writes + 1).flat_map(|n| (0..4).map(move |m| (n, m))) {
        let (mut v, mut l) = fresh();
        let at = v.store_mut().writes + n;
        v.store_mut().fail_after = Some(at);
        let _ = v.load_step(&mut l);
        let mut store = v.into_store();
        store.crash(|b| match mode {
            0 => false,
            1 => b % 3 == 0,
            2 => true,
            _ => b < 2,
        });
        let (mut m, report) =
            Volume::mount_checked(store).unwrap_or_else(|e| panic!("crash at write {n}, mode {mode}: mount: {e:?}"));
        if let Some(r) = report {
            assert!(r.is_clean(), "crash at write {n}, mode {mode}: ffsck: {:?}", r.findings);
        }
        let found = m.load_open().unwrap_or_else(|e| panic!("crash at write {n}, mode {mode}: {e}"));
        assert!(matches!(found.version, 3 | 4), "crash at write {n}, mode {mode}: version {}", found.version);
    }
}
