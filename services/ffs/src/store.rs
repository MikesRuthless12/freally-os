// SPDX-License-Identifier: Apache-2.0
//! Where the blocks live: drv-nvme's partition in the OS, a file on the
//! host. A write may stay in a cache until [`Store::flush`]; a crash loses
//! whatever wasn't flushed (and [`MemStore`] can lose it on purpose, for the
//! crash tests).

use crate::{BLOCK, FsError};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

pub trait Store {
    /// The volume's size in blocks.
    fn blocks(&self) -> u64;
    fn read(&mut self, block: u64, buf: &mut [u8; BLOCK]) -> Result<(), FsError>;
    fn write(&mut self, block: u64, buf: &[u8; BLOCK]) -> Result<(), FsError>;
    /// Blocks `first`, `first + 1` … written from `bufs`, in one go where
    /// the store can (TASK-105A: drv-nvme takes `RUN_BLOCKS` a message).
    fn write_run(&mut self, first: u64, bufs: &[&[u8; BLOCK]]) -> Result<(), FsError> {
        for (i, b) in bufs.iter().enumerate() {
            self.write(first + i as u64, b)?;
        }
        Ok(())
    }
    /// Everything written so far is durable when this returns.
    fn flush(&mut self) -> Result<(), FsError>;
}

/// The most blocks one run holds (the block protocol's `MAX_BLOCKS`).
pub const RUN_BLOCKS: usize = 15;

/// A volume in memory: what's durable, and what's written but not yet
/// flushed (which a crash may drop).
pub struct MemStore {
    durable: Vec<Box<[u8; BLOCK]>>,
    pending: BTreeMap<u64, Box<[u8; BLOCK]>>,
    /// Writes so far (the crash tests stop after the nth).
    pub writes: u64,
    /// Refuse every write after this many (a crash at that step).
    pub fail_after: Option<u64>,
}

impl MemStore {
    pub fn new(blocks: u64) -> MemStore {
        MemStore {
            durable: (0..blocks).map(|_| Box::new([0u8; BLOCK])).collect(),
            pending: BTreeMap::new(),
            writes: 0,
            fail_after: None,
        }
    }

    /// A crash: the unflushed writes are lost; `keep` says which survive
    /// anyway (a disk may have written some of its cache).
    pub fn crash(&mut self, mut keep: impl FnMut(u64) -> bool) {
        for (b, data) in core::mem::take(&mut self.pending) {
            if keep(b) {
                self.durable[b as usize] = data;
            }
        }
        self.fail_after = None;
    }

    /// The bytes of a block as a reader would see them now.
    pub fn peek(&self, block: u64) -> &[u8; BLOCK] {
        self.pending.get(&block).unwrap_or(&self.durable[block as usize])
    }

    /// Damage: flips a bit of a durable block.
    pub fn flip(&mut self, block: u64, byte: usize) {
        self.durable[block as usize][byte] ^= 1;
    }
}

impl Store for MemStore {
    fn blocks(&self) -> u64 {
        self.durable.len() as u64
    }

    fn read(&mut self, block: u64, buf: &mut [u8; BLOCK]) -> Result<(), FsError> {
        if block >= self.blocks() {
            return Err(FsError::Io);
        }
        buf.copy_from_slice(self.peek(block));
        Ok(())
    }

    fn write(&mut self, block: u64, buf: &[u8; BLOCK]) -> Result<(), FsError> {
        if block >= self.blocks() || self.fail_after.is_some_and(|n| self.writes >= n) {
            return Err(FsError::Io);
        }
        self.writes += 1;
        self.pending.insert(block, Box::new(*buf));
        Ok(())
    }

    fn flush(&mut self) -> Result<(), FsError> {
        if self.fail_after.is_some_and(|n| self.writes >= n) {
            return Err(FsError::Io);
        }
        for (b, data) in core::mem::take(&mut self.pending) {
            self.durable[b as usize] = data;
        }
        Ok(())
    }
}

