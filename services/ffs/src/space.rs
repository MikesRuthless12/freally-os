// SPDX-License-Identifier: Apache-2.0
//! Free space (§ 8), in memory: free extents by their first block, each with
//! the generation whose commit freed it. A block freed by generation `f` may
//! be written again only from generation `f + 2`, so nothing the generation
//! before the newest can reach is overwritten while the newest commits.
//! Freed generation 0 means "never written, or long enough ago": free to use.
//! The free-space tree on disk mirrors this at every commit (`volume.rs`).

use crate::{FIRST_FREE, FsError};
use alloc::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Space {
    /// start → (length, freed generation).
    free: BTreeMap<u64, (u64, u64)>,
}

impl Space {
    /// A new volume of `blocks`: everything from block 16 on.
    pub fn new_volume(blocks: u64) -> Space {
        let mut free = BTreeMap::new();
        free.insert(FIRST_FREE, (blocks - FIRST_FREE, 0));
        Space { free }
    }

    /// From the free-space tree's items.
    pub fn from_items(items: impl IntoIterator<Item = (u64, u64, u64)>) -> Space {
        Space { free: items.into_iter().map(|(s, l, g)| (s, (l, g))).collect() }
    }

    /// Every extent: (start, length, freed generation).
    pub fn items(&self) -> impl Iterator<Item = (u64, u64, u64)> + '_ {
        self.free.iter().map(|(&s, &(l, g))| (s, l, g))
    }

    pub fn free_blocks(&self) -> u64 {
        self.free.values().map(|e| e.0).sum()
    }

    pub fn is_free(&self, block: u64) -> bool {
        self.free.range(..=block).next_back().is_some_and(|(&s, &(l, _))| block < s + l)
    }

    /// Whether blocks freed by `freed` may be written in generation `generation`.
    fn usable(freed: u64, generation: u64) -> bool {
        freed == 0 || freed + 2 <= generation
    }

    /// A block for generation `generation`: the lowest one it may write.
    pub fn alloc(&mut self, generation: u64) -> Result<u64, FsError> {
        let (&start, &(len, freed)) =
            self.free.iter().find(|(_, e)| Self::usable(e.1, generation)).ok_or(FsError::NoSpace)?;
        self.free.remove(&start);
        if len > 1 {
            self.free.insert(start + 1, (len - 1, freed));
        }
        Ok(start)
    }

    /// `block` is free from now, freed by generation `freed` (0: it was never
    /// written, so it's free to use at once). Joins its neighbours when they
    /// were freed by the same generation.
    pub fn free(&mut self, block: u64, freed: u64) {
        debug_assert!(!self.is_free(block), "block {block} freed twice");
        let (mut start, mut len) = (block, 1);
        if let Some((&s, &(l, g))) = self.free.range(..block).next_back()
            && s + l == block
            && g == freed
        {
            self.free.remove(&s);
            (start, len) = (s, l + 1);
        }
        if let Some(&(l, g)) = self.free.get(&(block + 1))
            && g == freed
        {
            self.free.remove(&(block + 1));
            len += l;
        }
        self.free.insert(start, (len, freed));
    }

    /// The volume grown from `old` blocks to `new` (TASK-114): the new ones
    /// free to use, joined to a free run that ends at `old`.
    pub fn grow(&mut self, old: u64, new: u64) {
        if new <= old {
            return;
        }
        let (mut start, mut len) = (old, new - old);
        if let Some((&s, &(l, 0))) = self.free.range(..old).next_back()
            && s + l == old
        {
            self.free.remove(&s);
            (start, len) = (s, l + len);
        }
        self.free.insert(start, (len, 0));
    }

    /// At the start of generation `generation`: every extent it may write is
    /// marked free-to-use (0) and joined with its neighbours, so old frees
    /// don't fragment the tree.
    pub fn settle(&mut self, generation: u64) {
        let mut out: BTreeMap<u64, (u64, u64)> = BTreeMap::new();
        for (s, (l, g)) in core::mem::take(&mut self.free) {
            let g = if Self::usable(g, generation) { 0 } else { g };
            if let Some((&ps, pe)) = out.iter_mut().next_back()
                && ps + pe.0 == s
                && pe.1 == g
            {
                pe.0 += l;
                continue;
            }
            out.insert(s, (l, g));
        }
        self.free = out;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_freed_block_waits_two_generations() {
        let mut s = Space::new_volume(32);
        assert_eq!(s.alloc(1), Ok(16), "the lowest free block");
        assert_eq!(s.alloc(1), Ok(17));
        s.free(16, 5);
        assert_eq!(s.alloc(5), Ok(18), "not in the generation that freed it");
        assert_eq!(s.alloc(6), Ok(19), "nor the next");
        assert_eq!(s.alloc(7), Ok(16), "from the second after it");
        s.free(19, 0);
        assert_eq!(s.alloc(7), Ok(19), "never written: free at once");
    }

    #[test]
    fn neighbours_join_and_settle() {
        let mut s = Space::new_volume(20);
        for _ in 0..4 {
            s.alloc(1).unwrap();
        }
        assert_eq!(s.free_blocks(), 0);
        s.free(17, 3);
        s.free(16, 3);
        s.free(18, 4);
        assert_eq!(
            s.items().collect::<alloc::vec::Vec<_>>(),
            [(16, 2, 3), (18, 1, 4)],
            "different generations stay apart"
        );
        s.settle(6);
        assert_eq!(s.items().collect::<alloc::vec::Vec<_>>(), [(16, 3, 0)], "usable by 6: one extent");
        assert!(s.is_free(18) && !s.is_free(19));
        let mut full = Space::new_volume(17);
        full.alloc(1).unwrap();
        assert_eq!(full.alloc(1), Err(FsError::NoSpace));
    }
}
