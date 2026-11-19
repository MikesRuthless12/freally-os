// SPDX-License-Identifier: Apache-2.0
//! The disk asked in runs (TASK-105A): in the OS each call to the store is
//! a message to drv-nvme and a command to the disk, so a file's
//! consecutive blocks are read and written up to `RUN_BLOCKS` a call, never
//! one at a time; plain and sealed.

use svc_ffs::inode::{FileKind, ROOT_INO};
use svc_ffs::store::{MemStore, RUN_BLOCKS, Store};
use svc_ffs::subvol::SYSTEM;
use svc_ffs::volume::Volume;
use svc_ffs::{BLOCK, FsError};

/// A store in RAM that counts the calls made to it.
struct Counting {
    inner: MemStore,
    reads: u64,
    writes: u64,
}

impl Store for Counting {
    fn blocks(&self) -> u64 {
        self.inner.blocks()
    }
    fn read(&mut self, block: u64, buf: &mut [u8; BLOCK]) -> Result<(), FsError> {
        self.reads += 1;
        self.inner.read(block, buf)
    }
    fn read_run(&mut self, first: u64, bufs: &mut [Box<[u8; BLOCK]>]) -> Result<(), FsError> {
        self.reads += 1;
        for (i, b) in bufs.iter_mut().enumerate() {
            self.inner.read(first + i as u64, b)?;
        }
        Ok(())
    }
    fn write(&mut self, block: u64, buf: &[u8; BLOCK]) -> Result<(), FsError> {
        self.writes += 1;
        self.inner.write(block, buf)
    }
    fn write_run(&mut self, first: u64, bufs: &[&[u8; BLOCK]]) -> Result<(), FsError> {
        self.writes += 1;
        for (i, b) in bufs.iter().enumerate() {
            self.inner.write(first + i as u64, b)?;
        }
        Ok(())
    }
    fn flush(&mut self) -> Result<(), FsError> {
        self.inner.flush()
    }
}

fn runs(sealed: bool) {
    let store = Counting { inner: MemStore::new(4096), reads: 0, writes: 0 };
    let mut v = Volume::format(store, [3; 16]).unwrap();
    let sv = if sealed { v.create_encrypted_subvol("home", [9; 16], &[0x5A; 32]).unwrap() } else { SYSTEM };
    let f = v.create(sv, ROOT_INO, "one-mib.bin", FileKind::File).unwrap();
    let bytes: Vec<u8> = (0..1u32 << 20).map(|i| (i % 253) as u8).collect();
    v.write(sv, f, 0, &bytes).unwrap();
    let before = v.store_mut().writes;
    v.commit().unwrap();
    let blocks = (bytes.len() / BLOCK) as u64;
    let most = blocks.div_ceil(RUN_BLOCKS as u64) + 64;
    let wrote = v.store_mut().writes - before;
    assert!(wrote <= most, "the commit made {wrote} write calls for {blocks} blocks");
    // Read back from the store (the commit leaves nothing held).
    let before = v.store_mut().reads;
    assert_eq!(v.read(sv, f, 0, bytes.len() as u64).unwrap(), bytes);
    let read = v.store_mut().reads - before;
    assert!(read <= most, "reading {blocks} blocks made {read} read calls");
}

#[test]
fn a_file_is_read_and_written_in_runs() {
    runs(false);
}

#[test]
fn a_sealed_file_is_read_and_written_in_runs() {
    runs(true);
}
