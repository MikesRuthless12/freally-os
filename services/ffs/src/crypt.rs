// SPDX-License-Identifier: Apache-2.0
//! Encrypted subvolumes (§ 12, TASK-098): every node and data block of an
//! encrypted subvolume is sealed with XChaCha20-Poly1305 under its key, in
//! place (the same 4 KiB), its tag in the pointer that names it (a node's
//! `BlockPtr.check`, an extent's per-block check). The nonce is `block ‖
//! generation ‖ line`: copy-on-write writes a block once per generation, so
//! a nonce never repeats under one key. Names, sizes and the tree's shape
//! are inside the sealed nodes: nothing of the files is on the raw disk.
//!
//! The key (an account's, unwrapped at login by `accounts`) is held only
//! while the subvolume is unlocked; [`Volume::lock`] commits and wipes it.
//! The record's `key_check` (32 zero bytes sealed under the key) tells a
//! wrong key from a damaged tree.

use crate::store::Store;
use crate::volume::Volume;
use crate::{BLOCK, FsError};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use fcrypto::aead::{xopen, xseal};

/// A record's `kind` for an encrypted subvolume.
pub const ENCRYPTED: u8 = 1;

/// The nonce for `block` written by `generation` in `line` (§ 12).
pub fn nonce(block: u64, generation: u64, line: u64) -> [u8; 24] {
    let mut n = [0u8; 24];
    n[..8].copy_from_slice(&block.to_le_bytes());
    n[8..16].copy_from_slice(&generation.to_le_bytes());
    n[16..].copy_from_slice(&line.to_le_bytes());
    n
}

/// Overwrites a key so it doesn't outlive its use.
fn wipe(k: &mut [u8; 32]) {
    for b in k.iter_mut() {
        // SAFETY: a valid, aligned byte of `k`.
        unsafe { core::ptr::write_volatile(b, 0) };
    }
}

/// The keys held, by subvolume; each wiped when it goes, and all of them
/// when the volume does. Boxed, so the map's nodes hold only pointers: a
/// B-tree shifts its entries on insert and remove, and would leave stale
/// copies of a key behind.
#[derive(Default)]
pub(crate) struct Keys(pub(crate) BTreeMap<u64, Box<[u8; 32]>>);

impl Drop for Keys {
    fn drop(&mut self) {
        for k in self.0.values_mut() {
            wipe(k);
        }
    }
}

/// What sealing needs for a subvolume: its key and its line. The key is a
/// copy, wiped when the sealer goes.
pub(crate) struct Sealer {
    key: [u8; 32],
    line: u64,
}

impl Drop for Sealer {
    fn drop(&mut self) {
        wipe(&mut self.key);
    }
}

impl Sealer {
    /// Seals `data` (a whole block) in place for `block` and `generation`: its tag.
    pub(crate) fn seal(&self, block: u64, generation: u64, data: &mut [u8; BLOCK]) -> [u8; 16] {
        xseal(&self.key, &nonce(block, generation, self.line), &[], data)
    }

    /// Opens `data` in place, or `Integrity` (any change to the block, the
    /// tag, or the nonce's parts).
    pub(crate) fn open(
        &self,
        block: u64,
        generation: u64,
        data: &mut [u8; BLOCK],
        tag: &[u8; 16],
    ) -> Result<(), FsError> {
        xopen(&self.key, &nonce(block, generation, self.line), &[], data, tag).map_err(|_| FsError::Integrity(block))
    }
}

impl<S: Store> Volume<S> {
    /// How subvolume (tree) `tree`'s blocks are sealed: `None` when they
    /// aren't (a plain tree or subvolume); `Locked` without its key.
    pub(crate) fn sealer(&mut self, tree: u64) -> Result<Option<Sealer>, FsError> {
        if tree < crate::subvol::SYSTEM {
            return Ok(None);
        }
        let rec = &self.subvol(tree)?.rec;
        if rec.kind != ENCRYPTED {
            return Ok(None);
        }
        let line = if rec.snapshot_of != 0 { rec.snapshot_of } else { tree };
        let key = **self.keys.0.get(&tree).ok_or(FsError::Locked)?;
        Ok(Some(Sealer { key, line }))
    }

    /// A new encrypted subvolume called `name`, owned by `owner`, sealed
    /// under `key` (left unlocked). Its id.
    pub fn create_encrypted_subvol(&mut self, name: &str, owner: [u8; 16], key: &[u8; 32]) -> Result<u64, FsError> {
        let id = self.next_subvol_id();
        let mut check = [0u8; 32];
        let tag = xseal(key, &nonce(0, 0, id), &[], &mut check);
        self.keys.0.insert(id, Box::new(*key));
        self.make_subvol(id, name)?;
        {
            let s = self.subvol(id)?;
            s.rec.kind = ENCRYPTED;
            s.rec.owner = owner;
            s.rec.key_check[..32].copy_from_slice(&check);
            s.rec.key_check[32..].copy_from_slice(&tag);
        }
        let ino = self.take_ino(id)?;
        let mut root_dir = crate::inode::Inode::new(crate::inode::FileKind::Dir, ino, self.now);
        root_dir.nlink = 2;
        self.put_inode(id, ino, &root_dir)?;
        Ok(id)
    }

    /// Unlocks subvolume `sv` with `key`: `WrongKey` unless its key check opens.
    pub fn unlock(&mut self, sv: u64, key: &[u8; 32]) -> Result<(), FsError> {
        let rec = self.subvol(sv)?.rec.clone();
        if rec.kind != ENCRYPTED {
            return Ok(());
        }
        let line = if rec.snapshot_of != 0 { rec.snapshot_of } else { sv };
        let mut check = [0u8; 32];
        check.copy_from_slice(&rec.key_check[..32]);
        let mut tag = [0u8; 16];
        tag.copy_from_slice(&rec.key_check[32..]);
        if xopen(key, &nonce(0, 0, line), &[], &mut check, &tag).is_err() || check != [0; 32] {
            return Err(FsError::WrongKey);
        }
        self.keys.0.insert(sv, Box::new(*key));
        Ok(())
    }

    /// Commits, then forgets the key of subvolume `sv`'s line (wiped): its
    /// own, and its snapshots' (or, for a snapshot, its source's and theirs).
    pub fn lock(&mut self, sv: u64) -> Result<(), FsError> {
        self.commit()?;
        let line_of = |id: u64, r: &crate::subvol::SubvolRecord| if r.snapshot_of != 0 { r.snapshot_of } else { id };
        let line = line_of(sv, &self.subvol(sv)?.rec);
        let held: alloc::vec::Vec<u64> = self.keys.0.keys().copied().collect();
        for id in held {
            if line_of(id, &self.subvol(id)?.rec) == line
                && let Some(mut k) = self.keys.0.remove(&id)
            {
                wipe(&mut k);
            }
        }
        Ok(())
    }

    /// Whether subvolume `sv` is encrypted and its key isn't held.
    pub fn is_locked(&mut self, sv: u64) -> Result<bool, FsError> {
        Ok(self.subvol(sv)?.rec.kind == ENCRYPTED && !self.keys.0.contains_key(&sv))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nonce_is_block_generation_line() {
        let n = nonce(0x0102, 7, 300);
        assert_eq!(&n[..8], &0x0102u64.to_le_bytes());
        assert_eq!(&n[8..16], &7u64.to_le_bytes());
        assert_eq!(&n[16..], &300u64.to_le_bytes());
        let s = Sealer { key: [9; 32], line: 300 };
        let mut b = [0x5Au8; BLOCK];
        let tag = s.seal(40, 7, &mut b);
        assert_ne!(b, [0x5A; BLOCK], "sealed in place");
        let mut wrong = b;
        assert_eq!(s.open(41, 7, &mut wrong, &tag), Err(FsError::Integrity(41)), "another block's nonce");
        let mut wrong = b;
        assert_eq!(s.open(40, 8, &mut wrong, &tag), Err(FsError::Integrity(40)), "another generation's");
        s.open(40, 7, &mut b, &tag).unwrap();
        assert_eq!(b, [0x5A; BLOCK]);
    }
}
