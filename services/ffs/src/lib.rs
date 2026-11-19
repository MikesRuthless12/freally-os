// SPDX-License-Identifier: Apache-2.0
//! ffs: FreallyFS, the copy-on-write filesystem, to its specification
//! (`docs/ffs-format.md`, written first). This library is the format and its
//! algorithms, tested on the host against a file-backed disk; the service
//! (with vfs, TASK-100) puts it on drv-nvme.
//!
//! Part 1 (TASK-094): the superblocks, checksummed block pointers, the 4 KiB
//! nodes, the copy-on-write B+tree, and the free-space tree with its
//! allocator (a freed block waits two generations). Part 2 (TASK-095):
//! inodes, directories, extents and file data, and the transaction policy.
//! Part 3 (TASK-096): subvolumes, snapshots and reference counts, and
//! `ffsck`. Part 4 (TASK-098): encrypted subvolumes.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod btree;
pub mod crypt;
pub mod dir;
pub mod extent;
pub mod fsck;
pub mod inode;
pub mod node;
pub mod ptr;
pub mod snapshot;
pub mod space;
pub mod store;
pub mod subvol;
pub mod superblock;
pub mod txn;
pub mod volume;

/// Every block is 4 KiB.
pub const BLOCK: usize = 4096;
/// Blocks 0 and 1 are the superblock slots; 2–15 are reserved.
pub const FIRST_FREE: u64 = 16;

/// What went wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsError {
    /// The disk refused a read, write or flush.
    Io,
    /// A block isn't what its pointer says (its checksum, number or
    /// generation): damage, a lost write or a misdirected one.
    Integrity(u64),
    /// No valid superblock: not a FreallyFS volume.
    NotFreallyFs,
    /// No free block left to allocate.
    NoSpace,
    /// A value larger than an item may be (3,000 bytes).
    TooLarge,
    /// A structure that breaks the format (a bad node, a key out of order).
    Corrupt(&'static str),
    /// No such file, directory or item.
    NotFound,
    /// A name already in the directory.
    Exists,
    /// A name FreallyFS doesn't take (empty, over 255 bytes, `/`, NUL, `.`, `..`).
    BadName,
    /// A directory that still holds entries.
    NotEmpty,
    /// A file where a directory must be.
    NotADirectory,
    /// A directory where a file must be.
    IsADirectory,
    /// A read-only subvolume (a snapshot), or one that can't go (the system's).
    ReadOnly,
    /// An encrypted subvolume whose key isn't held.
    Locked,
    /// A key that doesn't open the subvolume's key check.
    WrongKey,
}

/// A key: `(object, kind, offset)`, compared in that order (§ 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Key {
    pub object: u64,
    pub kind: u8,
    pub offset: u64,
}

impl Key {
    pub const fn new(object: u64, kind: u8, offset: u64) -> Key {
        Key { object, kind, offset }
    }

    pub const LEN: usize = 17;

    pub fn bytes(&self) -> [u8; 17] {
        let mut b = [0u8; 17];
        b[..8].copy_from_slice(&self.object.to_le_bytes());
        b[8] = self.kind;
        b[9..].copy_from_slice(&self.offset.to_le_bytes());
        b
    }

    pub fn from_bytes(b: &[u8]) -> Key {
        let u64_at = |o: usize| {
            let mut a = [0u8; 8];
            a.copy_from_slice(&b[o..o + 8]);
            u64::from_le_bytes(a)
        };
        Key { object: u64_at(0), kind: b[8], offset: u64_at(9) }
    }
}

/// Item kinds (§ 10).
pub mod kind {
    pub const INODE: u8 = 1;
    pub const DIR: u8 = 2;
    pub const EXTENT: u8 = 3;
    pub const SUBVOL: u8 = 16;
    pub const FREE: u8 = 17;
    pub const REF: u8 = 18;
}

/// Tree ids (§ 6).
pub mod tree {
    pub const ROOT: u64 = 1;
    pub const FREE: u64 = 2;
    pub const REF: u64 = 3;
}
