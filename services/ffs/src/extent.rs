// SPDX-License-Identifier: Apache-2.0
//! File data (§ 11): extents under `(inode, EXTENT, file offset)`, each a
//! run of 1–64 blocks contiguous on disk, with a check per block (CRC32C
//! here; the Poly1305 tag in an encrypted subvolume, TASK-098). Data is
//! copy-on-write like the nodes: a block the last generation can reach is
//! never written again; a block this generation wrote stays in memory and
//! changes in place until the commit writes it, first (§ 13 step 1). A hole
//! reads as zeros.

use crate::inode::FileKind;
use crate::ptr::BlockPtr;
use crate::store::Store;
use crate::volume::Volume;
use crate::{BLOCK, FsError, Key, kind};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// The most blocks one extent holds.
pub const MAX_BLOCKS: u64 = 64;
const B: u64 = BLOCK as u64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extent {
    pub block: u64,
    /// The bytes of its last block in use (1–4,096).
    pub len: u32,
    pub checks: Vec<[u8; 16]>,
}

impl Extent {
    pub fn count(&self) -> u64 {
        self.checks.len() as u64
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16 + 16 * self.checks.len());
        out.extend_from_slice(&self.block.to_le_bytes());
        out.extend_from_slice(&(self.checks.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.len.to_le_bytes());
        for c in &self.checks {
            out.extend_from_slice(c);
        }
        out
    }

    pub fn decode(b: &[u8]) -> Result<Extent, FsError> {
        if b.len() < 16 {
            return Err(FsError::Corrupt("an extent cut short"));
        }
        let block = u64::from_le_bytes(b[..8].try_into().unwrap_or_default());
        let count = u32::from_le_bytes(b[8..12].try_into().unwrap_or_default()) as usize;
        let len = u32::from_le_bytes(b[12..16].try_into().unwrap_or_default());
        if count == 0 || count as u64 > MAX_BLOCKS || b.len() != 16 + 16 * count || !(1..=4096).contains(&len) {
            return Err(FsError::Corrupt("an extent's size"));
        }
        let checks = b[16..].as_chunks::<16>().0.to_vec();
        Ok(Extent { block, len, checks })
    }
}

/// A file block's place on disk and its check.
type Mapped = BTreeMap<u64, (u64, [u8; 16])>;

impl<S: Store> Volume<S> {
    /// The blocks of file `ino` from file block `first` to `last`
    /// (inclusive) that have data, and the extents' keys that hold them.
    fn mapped(&mut self, sv: u64, ino: u64, first: u64, last: u64) -> Result<(Mapped, Vec<(Key, Extent)>), FsError> {
        let root = self.sv_root(sv)?;
        let from = Key::new(ino, kind::EXTENT, first.saturating_sub(MAX_BLOCKS - 1) * B);
        let mut raw = Vec::new();
        self.tree_scan(&root, sv, &from, &mut |k, v| {
            if k.object != ino || k.kind != kind::EXTENT || k.offset > last * B {
                return false;
            }
            raw.push((k, v.to_vec()));
            true
        })?;
        let mut map = Mapped::new();
        let mut holding = Vec::new();
        for (k, v) in raw {
            let e = Extent::decode(&v)?;
            let start = k.offset / B;
            if start + e.count() <= first {
                continue;
            }
            for (i, c) in e.checks.iter().enumerate() {
                map.insert(start + i as u64, (e.block + i as u64, *c));
            }
            holding.push((k, e));
        }
        Ok((map, holding))
    }

    /// A data block's bytes, checked against `check`.
    fn read_data(&mut self, block: u64, check: &[u8; 16]) -> Result<Box<[u8; BLOCK]>, FsError> {
        if let Some(d) = self.data.get(&block) {
            return Ok(d.clone());
        }
        let mut b = Box::new([0u8; BLOCK]);
        self.store.read(block, &mut b)?;
        if BlockPtr::plain_check(&b) != *check {
            return Err(FsError::Integrity(block));
        }
        Ok(b)
    }

    /// Replaces the extents in `holding` with extents for `map` (file block
    /// → disk block and check), keeping whatever of them lies outside
    /// `range` (first and last file block): runs contiguous on disk join, up
    /// to 64 blocks.
    fn rewrite_extents(
        &mut self,
        sv: u64,
        ino: u64,
        holding: Vec<(Key, Extent)>,
        mut map: Mapped,
        (first, last): (u64, u64),
        size: u64,
    ) -> Result<(), FsError> {
        for (k, e) in &holding {
            let start = k.offset / B;
            for i in 0..e.count() {
                let fb = start + i;
                if fb < first || fb > last {
                    map.entry(fb).or_insert((e.block + i, e.checks[i as usize]));
                }
            }
            self.sv_delete(sv, k)?;
        }
        let last_fb = size.div_ceil(B).saturating_sub(1);
        let tail = ((size.max(1) - 1) % B + 1) as u32;
        let mut run: Option<(u64, Extent)> = None;
        let flush = |v: &mut Self, run: &mut Option<(u64, Extent)>| -> Result<(), FsError> {
            if let Some((start, mut e)) = run.take() {
                e.len = if start + e.count() - 1 == last_fb { tail } else { 4096 };
                v.sv_insert(sv, Key::new(ino, kind::EXTENT, start * B), &e.encode())?;
            }
            Ok(())
        };
        for (fb, (block, check)) in map {
            let joins = run
                .as_ref()
                .is_some_and(|(s, e)| s + e.count() == fb && e.block + e.count() == block && e.count() < MAX_BLOCKS);
            if joins {
                if let Some((_, e)) = run.as_mut() {
                    e.checks.push(check);
                }
            } else {
                flush(self, &mut run)?;
                run = Some((fb, Extent { block, len: 4096, checks: alloc::vec![check] }));
            }
        }
        flush(self, &mut run)
    }

    /// Writes `data` at byte `offset` of file `ino`.
    pub fn write(&mut self, sv: u64, ino: u64, offset: u64, data: &[u8]) -> Result<(), FsError> {
        self.writable(sv)?;
        let mut inode = self.stat(sv, ino)?;
        if inode.kind != FileKind::File {
            return Err(FsError::IsADirectory);
        }
        if data.is_empty() {
            return Ok(());
        }
        let end = offset.checked_add(data.len() as u64).filter(|&e| e <= 1 << 44).ok_or(FsError::TooLarge)?;
        let (first, last) = (offset / B, (end - 1) / B);
        // The old tail's extent changes its `len` when the file grows past it.
        let old_tail = inode.size.div_ceil(B).saturating_sub(1);
        let (lo, hi) = (first.min(old_tail), last.max(old_tail));
        let (mut map, holding) = self.mapped(sv, ino, lo, hi)?;
        // Blocks this write replaces: released once the extents have
        // changed (a shared leaf's copy gives them a reference first).
        let mut replaced = Vec::new();
        for fb in first..=last {
            let mut buf = match map.get(&fb) {
                Some(&(block, check)) => self.read_data(block, &check)?,
                None => Box::new([0u8; BLOCK]),
            };
            let (a, b) = ((fb * B).max(offset), ((fb + 1) * B).min(end));
            buf[(a - fb * B) as usize..(b - fb * B) as usize]
                .copy_from_slice(&data[(a - offset) as usize..(b - offset) as usize]);
            let check = BlockPtr::plain_check(&buf);
            let block = match map.get(&fb) {
                Some(&(old, _)) if self.data.contains_key(&old) => old,
                Some(&(old, _)) => {
                    replaced.push(old);
                    self.space.alloc(self.generation)?
                }
                None => self.space.alloc(self.generation)?,
            };
            self.data.insert(block, buf);
            map.insert(fb, (block, check));
        }
        inode.size = inode.size.max(end);
        self.rewrite_extents(sv, ino, holding, map, (lo, hi), inode.size)?;
        for b in replaced {
            self.release_data(b)?;
        }
        (inode.modified_ns, inode.changed_ns) = (self.now, self.now);
        self.put_inode(sv, ino, &inode)
    }

    /// Up to `len` bytes of file `ino` from `offset` (fewer at its end).
    pub fn read(&mut self, sv: u64, ino: u64, offset: u64, len: u64) -> Result<Vec<u8>, FsError> {
        let inode = self.stat(sv, ino)?;
        if inode.kind != FileKind::File {
            return Err(FsError::IsADirectory);
        }
        let end = offset.saturating_add(len).min(inode.size);
        if offset >= end {
            return Ok(Vec::new());
        }
        let (first, last) = (offset / B, (end - 1) / B);
        let (map, _) = self.mapped(sv, ino, first, last)?;
        let mut out = Vec::with_capacity((end - offset) as usize);
        for fb in first..=last {
            let (a, b) = ((fb * B).max(offset), ((fb + 1) * B).min(end));
            match map.get(&fb) {
                Some(&(block, check)) => {
                    let buf = self.read_data(block, &check)?;
                    out.extend_from_slice(&buf[(a - fb * B) as usize..(b - fb * B) as usize]);
                }
                None => out.resize(out.len() + (b - a) as usize, 0),
            }
        }
        Ok(out)
    }

    /// Sets file `ino`'s size: shrinking frees the blocks past the end and
    /// zeroes the rest of the new last block; growing leaves a hole.
    pub fn truncate(&mut self, sv: u64, ino: u64, size: u64) -> Result<(), FsError> {
        self.writable(sv)?;
        let mut inode = self.stat(sv, ino)?;
        if inode.kind != FileKind::File {
            return Err(FsError::IsADirectory);
        }
        if size < inode.size {
            let keep = size.div_ceil(B);
            let old_last = inode.size.div_ceil(B) - 1;
            let lo = keep.saturating_sub(1);
            let (map, holding) = self.mapped(sv, ino, lo, old_last)?;
            let mut kept = Mapped::new();
            let mut gone = Vec::new();
            for (fb, (block, check)) in map {
                if fb >= keep {
                    gone.push(block);
                } else {
                    kept.insert(fb, (block, check));
                }
            }
            // The new last block's bytes past the end become zeros.
            if !size.is_multiple_of(B)
                && let Some(&(block, check)) = kept.get(&lo)
            {
                let mut buf = self.read_data(block, &check)?;
                buf[(size % B) as usize..].fill(0);
                let nb = if self.data.contains_key(&block) {
                    block
                } else {
                    gone.push(block);
                    self.space.alloc(self.generation)?
                };
                let c = BlockPtr::plain_check(&buf);
                self.data.insert(nb, buf);
                kept.insert(lo, (nb, c));
            }
            // Holding past `lo`: rebuilt from what's kept (all at or before `lo`).
            self.rewrite_extents(sv, ino, holding, kept, (lo, old_last), size)?;
            for b in gone {
                self.release_data(b)?;
            }
        } else if size > inode.size && inode.size > 0 {
            // The old tail's `len` becomes a whole block (or the new tail's, if the same).
            let old_last = inode.size.div_ceil(B) - 1;
            let (map, holding) = self.mapped(sv, ino, old_last, old_last)?;
            let only: Mapped = map.into_iter().filter(|(fb, _)| *fb == old_last).collect();
            self.rewrite_extents(sv, ino, holding, only, (old_last, old_last), size)?;
        }
        inode.size = size;
        (inode.modified_ns, inode.changed_ns) = (self.now, self.now);
        self.put_inode(sv, ino, &inode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_extent_round_trips() {
        let e = Extent { block: 4000, len: 100, checks: alloc::vec![[1; 16], [2; 16], [3; 16]] };
        assert_eq!(Extent::decode(&e.encode()).unwrap(), e);
        let mut bad = e.encode();
        bad[12] = 0;
        bad[13] = 0x20; // len 8192
        assert!(Extent::decode(&bad).is_err());
        assert_eq!(e.encode().len(), 16 + 48);
        let full = Extent { block: 0, len: 4096, checks: alloc::vec![[0; 16]; 64] };
        assert!(full.encode().len() <= crate::node::MAX_VALUE);
    }
}
