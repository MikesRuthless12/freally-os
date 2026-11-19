// SPDX-License-Identifier: Apache-2.0
//! How fast FreallyFS writes (TASK-105A): a file written in 60 KiB pieces,
//! as vfs sends them, plain and sealed, on a store in RAM. Run by hand:
//! `cargo test -p svc-ffs --release --test speed -- --ignored --nocapture`.

use std::time::Instant;
use svc_ffs::inode::{FileKind, ROOT_INO};
use svc_ffs::store::MemStore;
use svc_ffs::subvol::SYSTEM;
use svc_ffs::volume::Volume;

fn write_mib(sealed: bool, mib: usize) -> f64 {
    let mut v = Volume::format(MemStore::new(32_768), [1; 16]).unwrap();
    let sv = if sealed { v.create_encrypted_subvol("home", [7; 16], &[0x4B; 32]).unwrap() } else { SYSTEM };
    let f = v.create(sv, ROOT_INO, "big.bin", FileKind::File).unwrap();
    let piece: Vec<u8> = (0..60 * 1024).map(|i| (i % 251) as u8).collect();
    let total = mib << 20;
    let start = Instant::now();
    let mut at = 0;
    while at < total {
        let n = piece.len().min(total - at);
        v.write(sv, f, at as u64, &piece[..n]).unwrap();
        at += n;
        if v.should_commit(0) {
            v.commit().unwrap();
        }
    }
    v.commit().unwrap();
    mib as f64 / start.elapsed().as_secs_f64()
}

#[test]
#[ignore]
fn write_speed() {
    for mib in [4, 16, 32] {
        println!("{mib} MiB: plain {:.1} MiB/s, sealed {:.1} MiB/s", write_mib(false, mib), write_mib(true, mib));
    }
}
