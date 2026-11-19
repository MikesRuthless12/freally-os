// SPDX-License-Identifier: Apache-2.0
//! Inodes (§ 11): 64 bytes under `(inode, INODE, 0)` in their subvolume's
//! tree: the kind, the links, the size, the directory above, and three
//! times. A subvolume's root directory is inode 256.

use crate::store::Store;
use crate::volume::Volume;
use crate::{FsError, Key, kind};

pub const ROOT_INO: u64 = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    File = 1,
    Dir = 2,
}

impl FileKind {
    pub fn from_u8(v: u8) -> Result<FileKind, FsError> {
        match v {
            1 => Ok(FileKind::File),
            2 => Ok(FileKind::Dir),
            _ => Err(FsError::Corrupt("an inode of no known kind")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Inode {
    pub kind: FileKind,
    /// Bit 0: read-only.
    pub flags: u8,
    pub nlink: u32,
    /// Files: bytes. Directories: entries.
    pub size: u64,
    /// Directories: the directory above (the root: itself).
    pub parent: u64,
    pub created_ns: u64,
    pub modified_ns: u64,
    pub changed_ns: u64,
}

impl Inode {
    pub const LEN: usize = 64;

    pub fn new(kind: FileKind, parent: u64, now: u64) -> Inode {
        Inode { kind, flags: 0, nlink: 1, size: 0, parent, created_ns: now, modified_ns: now, changed_ns: now }
    }

    pub fn encode(&self) -> [u8; 64] {
        let mut b = [0u8; 64];
        b[0] = self.kind as u8;
        b[1] = self.flags;
        b[4..8].copy_from_slice(&self.nlink.to_le_bytes());
        for (at, v) in
            [(8, self.size), (16, self.parent), (24, self.created_ns), (32, self.modified_ns), (40, self.changed_ns)]
        {
            b[at..at + 8].copy_from_slice(&v.to_le_bytes());
        }
        b
    }

    pub fn decode(b: &[u8]) -> Result<Inode, FsError> {
        if b.len() != Self::LEN {
            return Err(FsError::Corrupt("an inode's length"));
        }
        let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap_or_default());
        Ok(Inode {
            kind: FileKind::from_u8(b[0])?,
            flags: b[1],
            nlink: u32::from_le_bytes(b[4..8].try_into().unwrap_or_default()),
            size: u64_at(8),
            parent: u64_at(16),
            created_ns: u64_at(24),
            modified_ns: u64_at(32),
            changed_ns: u64_at(40),
        })
    }
}

impl<S: Store> Volume<S> {
    /// Inode `ino` of subvolume `sv`.
    pub fn stat(&mut self, sv: u64, ino: u64) -> Result<Inode, FsError> {
        let raw = self.sv_get(sv, &Key::new(ino, kind::INODE, 0))?.ok_or(FsError::NotFound)?;
        Inode::decode(&raw)
    }

    pub(crate) fn put_inode(&mut self, sv: u64, ino: u64, i: &Inode) -> Result<(), FsError> {
        self.sv_insert(sv, Key::new(ino, kind::INODE, 0), &i.encode())
    }

    /// The next inode number of subvolume `sv`, taken.
    pub(crate) fn take_ino(&mut self, sv: u64) -> Result<u64, FsError> {
        let s = self.subvol(sv)?;
        let ino = s.rec.next_inode;
        s.rec.next_inode += 1;
        s.changed = true;
        Ok(ino)
    }
}
