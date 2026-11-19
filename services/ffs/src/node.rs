// SPDX-License-Identifier: Apache-2.0
//! Nodes (§ 5): one 4 KiB block each. A 64-byte header (magic `FFSN`,
//! level, count, tree, generation, its own block); then, in an internal
//! node, `(key, BlockPtr)` entries; in a leaf, `(key, value offset, value
//! length)` headers with the values packed from the block's end downward.

use crate::ptr::BlockPtr;
use crate::{BLOCK, FsError, Key};
use alloc::vec::Vec;

pub const MAGIC: [u8; 4] = *b"FFSN";
pub const HEADER: usize = 64;
/// An internal entry: a key and a pointer.
pub const ENTRY: usize = Key::LEN + BlockPtr::LEN;
/// A leaf item's header: a key, the value's offset and length.
pub const ITEM: usize = Key::LEN + 4;
/// The most children an internal node holds.
pub const MAX_CHILDREN: usize = (BLOCK - HEADER) / ENTRY;
/// The largest value an item may hold.
pub const MAX_VALUE: usize = 3000;
/// A tree's greatest height.
pub const MAX_LEVEL: u8 = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    /// Items, sorted by key.
    Leaf(Vec<(Key, Vec<u8>)>),
    /// Children: each key the least of its child's keys (a lower bound
    /// after deletions; the first is the subtree's least).
    Internal(Vec<(Key, BlockPtr)>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub level: u8,
    pub tree: u64,
    pub generation: u64,
    pub block: u64,
    pub body: Body,
}

impl Node {
    pub fn leaf(tree: u64) -> Node {
        Node { level: 0, tree, generation: 0, block: 0, body: Body::Leaf(Vec::new()) }
    }

    /// The bytes it takes, header included.
    pub fn used(&self) -> usize {
        HEADER
            + match &self.body {
                Body::Leaf(items) => items.iter().map(|(_, v)| ITEM + v.len()).sum::<usize>(),
                Body::Internal(children) => children.len() * ENTRY,
            }
    }

    pub fn fits(&self) -> bool {
        self.used() <= BLOCK
    }

    pub fn count(&self) -> usize {
        match &self.body {
            Body::Leaf(items) => items.len(),
            Body::Internal(children) => children.len(),
        }
    }

    /// Its least key (the first), if it has any.
    pub fn first_key(&self) -> Option<Key> {
        match &self.body {
            Body::Leaf(items) => items.first().map(|i| i.0),
            Body::Internal(children) => children.first().map(|c| c.0),
        }
    }

    pub fn encode(&self) -> Result<[u8; BLOCK], FsError> {
        if !self.fits() {
            return Err(FsError::Corrupt("a node larger than a block"));
        }
        let mut b = [0u8; BLOCK];
        b[..4].copy_from_slice(&MAGIC);
        b[4] = self.level;
        b[6..8].copy_from_slice(&(self.count() as u16).to_le_bytes());
        b[8..16].copy_from_slice(&self.tree.to_le_bytes());
        b[16..24].copy_from_slice(&self.generation.to_le_bytes());
        b[24..32].copy_from_slice(&self.block.to_le_bytes());
        match &self.body {
            Body::Internal(children) => {
                for (i, (k, p)) in children.iter().enumerate() {
                    let at = HEADER + i * ENTRY;
                    b[at..at + Key::LEN].copy_from_slice(&k.bytes());
                    b[at + Key::LEN..at + ENTRY].copy_from_slice(&p.bytes());
                }
            }
            Body::Leaf(items) => {
                let mut end = BLOCK;
                for (i, (k, v)) in items.iter().enumerate() {
                    end -= v.len();
                    b[end..end + v.len()].copy_from_slice(v);
                    let at = HEADER + i * ITEM;
                    b[at..at + Key::LEN].copy_from_slice(&k.bytes());
                    b[at + Key::LEN..at + Key::LEN + 2].copy_from_slice(&(end as u16).to_le_bytes());
                    b[at + Key::LEN + 2..at + ITEM].copy_from_slice(&(v.len() as u16).to_le_bytes());
                }
            }
        }
        Ok(b)
    }

    /// Reads a node, checking its header against where it was found
    /// (`block`), the tree it belongs to, and its own layout. The pointer's
    /// checksum is the caller's to check first.
    pub fn decode(b: &[u8; BLOCK], block: u64, tree: u64) -> Result<Node, FsError> {
        let u64_at = |o: usize| {
            let mut a = [0u8; 8];
            a.copy_from_slice(&b[o..o + 8]);
            u64::from_le_bytes(a)
        };
        if b[..4] != MAGIC || u64_at(24) != block || u64_at(8) != tree || b[4] > MAX_LEVEL {
            return Err(FsError::Integrity(block));
        }
        let (level, count) = (b[4], u16::from_le_bytes([b[6], b[7]]) as usize);
        let body = if level > 0 {
            if count == 0 || count > MAX_CHILDREN {
                return Err(FsError::Corrupt("an internal node's count"));
            }
            Body::Internal(
                (0..count)
                    .map(|i| {
                        let at = HEADER + i * ENTRY;
                        (Key::from_bytes(&b[at..at + Key::LEN]), BlockPtr::from_bytes(&b[at + Key::LEN..at + ENTRY]))
                    })
                    .collect(),
            )
        } else {
            if HEADER + count * ITEM > BLOCK {
                return Err(FsError::Corrupt("a leaf's count"));
            }
            let mut items = Vec::with_capacity(count);
            for i in 0..count {
                let at = HEADER + i * ITEM;
                let off = u16::from_le_bytes([b[at + Key::LEN], b[at + Key::LEN + 1]]) as usize;
                let len = u16::from_le_bytes([b[at + Key::LEN + 2], b[at + Key::LEN + 3]]) as usize;
                if off < HEADER + count * ITEM || off + len > BLOCK || len > MAX_VALUE {
                    return Err(FsError::Corrupt("a leaf item's value"));
                }
                items.push((Key::from_bytes(&b[at..at + Key::LEN]), b[off..off + len].to_vec()));
            }
            Body::Leaf(items)
        };
        let n = Node { level, tree, generation: u64_at(16), block, body };
        let sorted = match &n.body {
            Body::Leaf(items) => items.windows(2).all(|w| w[0].0 < w[1].0),
            Body::Internal(children) => children.windows(2).all(|w| w[0].0 < w[1].0),
        };
        if !sorted {
            return Err(FsError::Corrupt("keys out of order"));
        }
        Ok(n)
    }

    /// Splits a node that doesn't fit into two halves by bytes, the left
    /// staying here. Returns the right half.
    pub fn split(&mut self) -> Node {
        let right_body = match &mut self.body {
            Body::Leaf(items) => {
                let total: usize = items.iter().map(|(_, v)| ITEM + v.len()).sum();
                let mut acc = 0;
                let mut at = items.len();
                for (i, (_, v)) in items.iter().enumerate() {
                    acc += ITEM + v.len();
                    if acc * 2 >= total {
                        at = (i + 1).min(items.len() - 1).max(1);
                        break;
                    }
                }
                Body::Leaf(items.split_off(at))
            }
            Body::Internal(children) => {
                let at = children.len() / 2;
                Body::Internal(children.split_off(at))
            }
        };
        Node { level: self.level, tree: self.tree, generation: self.generation, block: 0, body: right_body }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_and_internal_nodes_round_trip() {
        let mut leaf = Node::leaf(5);
        leaf.block = 42;
        leaf.generation = 3;
        leaf.body =
            Body::Leaf(alloc::vec![(Key::new(1, 1, 0), b"one".to_vec()), (Key::new(2, 1, 0), alloc::vec![7; 3000])]);
        let b = leaf.encode().unwrap();
        assert_eq!(Node::decode(&b, 42, 5).unwrap(), leaf);
        assert_eq!(Node::decode(&b, 43, 5), Err(FsError::Integrity(43)), "found at another block");
        assert_eq!(Node::decode(&b, 42, 6), Err(FsError::Integrity(42)), "in another tree");
        let p = BlockPtr { block: 9, generation: 2, check: [1; 16] };
        let inner = Node {
            level: 1,
            tree: 5,
            generation: 3,
            block: 43,
            body: Body::Internal((0..MAX_CHILDREN as u64).map(|i| (Key::new(i, 0, 0), p)).collect()),
        };
        assert!(inner.fits());
        assert_eq!(Node::decode(&inner.encode().unwrap(), 43, 5).unwrap(), inner);
        assert_eq!(MAX_CHILDREN, 82);
    }

    #[test]
    fn a_split_halves_by_bytes_and_keeps_order() {
        let mut n = Node::leaf(1);
        n.body = Body::Leaf((0..40u64).map(|i| (Key::new(i, 1, 0), alloc::vec![0; 120])).collect());
        assert!(!n.fits());
        let r = n.split();
        assert!(n.fits() && r.fits());
        assert_eq!(n.count() + r.count(), 40);
        assert!(n.first_key() < r.first_key());
        assert!((n.count() as i32 - r.count() as i32).abs() <= 1);
    }
}
