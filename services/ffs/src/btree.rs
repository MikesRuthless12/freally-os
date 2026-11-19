// SPDX-License-Identifier: Apache-2.0
//! The copy-on-write B+tree (§ 5): a node the committed generation can
//! reach is never changed; changing it copies it to a new block (its old
//! block freed by this generation), and its parent's pointer with it, up to
//! the root. A node already copied in this generation changes in place, in
//! memory; nothing reaches the disk until the commit (`volume.rs`), which
//! writes each dirty node after its children, so every pointer carries the
//! checksum of what it points to.

use crate::node::{Body, MAX_LEVEL, MAX_VALUE, Node};
use crate::ptr::BlockPtr;
use crate::store::Store;
use crate::volume::Volume;
use crate::{BLOCK, FsError, Key};
use alloc::vec;
use alloc::vec::Vec;

/// The child of an internal node to descend into for `key`: the last whose
/// key is at most `key` (the first, for a key below them all).
/// What a delete leaves a node's parent: the node's new entry (none if it
/// emptied) and the value taken out (none if the key wasn't there).
type Deleted = (Option<(Key, BlockPtr)>, Option<Vec<u8>>);

fn child_index(children: &[(Key, BlockPtr)], key: &Key) -> usize {
    children.partition_point(|(k, _)| k <= key).saturating_sub(1)
}

/// A node's items packed into nodes that each fit: halves when two fit,
/// else as many as it takes, left to right.
fn split_to_fit(mut n: Node) -> Vec<Node> {
    if n.fits() {
        return vec![n];
    }
    let right = n.split();
    if n.fits() && right.fits() {
        return vec![n, right];
    }
    // Rejoin and pack greedily (a big value beside many small ones).
    let Body::Leaf(mut items) = n.body else { return vec![n, right] };
    if let Body::Leaf(r) = right.body {
        items.extend(r);
    }
    let (level, tree, generation) = (n.level, n.tree, n.generation);
    let mut out: Vec<Node> = Vec::new();
    let mut cur = Node { level, tree, generation, block: 0, body: Body::Leaf(Vec::new()) };
    for it in items {
        let Body::Leaf(v) = &mut cur.body else { unreachable!() };
        v.push(it);
        if !cur.fits() {
            let Body::Leaf(v) = &mut cur.body else { unreachable!() };
            let last = v.pop().expect("just pushed");
            let full =
                core::mem::replace(&mut cur, Node { level, tree, generation, block: 0, body: Body::Leaf(vec![last]) });
            out.push(full);
        }
    }
    out.push(cur);
    out
}

impl<S: Store> Volume<S> {
    /// The node `p` points to in `tree`: this generation's copy if it has
    /// one, else read from the disk and checked against the pointer (its
    /// checksum, block and generation).
    pub(crate) fn load(&mut self, p: &BlockPtr, tree: u64) -> Result<Node, FsError> {
        if p.generation == self.generation
            && let Some(n) = self.dirty.get(&p.block)
        {
            return Ok(n.clone());
        }
        let mut b = [0u8; BLOCK];
        self.store.read(p.block, &mut b)?;
        if !p.matches(&b) {
            return Err(FsError::Integrity(p.block));
        }
        let n = Node::decode(&b, p.block, tree)?;
        if n.generation != p.generation {
            return Err(FsError::Integrity(p.block));
        }
        Ok(n)
    }

    /// Keeps `n` as this generation's copy of what `p` pointed to: in place
    /// if `p` is already this generation's, else in a new block (the old one
    /// freed by this generation). Returns the pointer to it.
    pub(crate) fn cow(&mut self, p: Option<&BlockPtr>, mut n: Node) -> Result<BlockPtr, FsError> {
        let block = match p {
            Some(p) if p.generation == self.generation && self.dirty.contains_key(&p.block) => p.block,
            Some(p) => {
                let b = self.space.alloc(self.generation)?;
                self.space.free(p.block, self.generation);
                b
            }
            None => self.space.alloc(self.generation)?,
        };
        n.block = block;
        n.generation = self.generation;
        self.dirty.insert(block, n);
        Ok(BlockPtr { block, generation: self.generation, check: [0; 16] })
    }

    /// The node `p` pointed to is gone (emptied or merged away).
    fn discard(&mut self, p: &BlockPtr) {
        if p.generation == self.generation && self.dirty.remove(&p.block).is_some() {
            // Never written: free to use again at once.
            self.space.free(p.block, 0);
        } else {
            self.space.free(p.block, self.generation);
        }
    }

    /// The value under `key` in the tree rooted at `root`.
    pub fn tree_get(&mut self, root: &BlockPtr, tree: u64, key: &Key) -> Result<Option<Vec<u8>>, FsError> {
        let mut p = *root;
        loop {
            let n = self.load(&p, tree)?;
            match n.body {
                Body::Internal(children) => p = children[child_index(&children, key)].1,
                Body::Leaf(items) => {
                    return Ok(items.binary_search_by(|(k, _)| k.cmp(key)).ok().map(|i| items[i].1.clone()));
                }
            }
        }
    }

    /// Puts `value` under `key` (replacing any), returning the new root.
    pub fn tree_insert(&mut self, root: &BlockPtr, tree: u64, key: Key, value: &[u8]) -> Result<BlockPtr, FsError> {
        if value.len() > MAX_VALUE {
            return Err(FsError::TooLarge);
        }
        let level = self.load(root, tree)?.level;
        let mut out = self.insert_at(root, tree, key, value)?;
        if out.len() == 1 {
            return Ok(out.remove(0).1);
        }
        // The root split: a new root above the halves.
        if level + 1 > MAX_LEVEL {
            return Err(FsError::Corrupt("a tree taller than 8 levels"));
        }
        let mut new_root = Node::leaf(tree);
        new_root.level = level + 1;
        new_root.body = Body::Internal(out);
        self.cow(None, new_root)
    }

    fn insert_at(&mut self, p: &BlockPtr, tree: u64, key: Key, value: &[u8]) -> Result<Vec<(Key, BlockPtr)>, FsError> {
        let mut n = self.load(p, tree)?;
        match &mut n.body {
            Body::Leaf(items) => match items.binary_search_by(|(k, _)| k.cmp(&key)) {
                Ok(i) => items[i].1 = value.to_vec(),
                Err(i) => items.insert(i, (key, value.to_vec())),
            },
            Body::Internal(children) => {
                let i = child_index(children, &key);
                let child = children[i].1;
                let below = self.insert_at(&child, tree, key, value)?;
                children.splice(i..=i, below);
            }
        }
        self.write_back(p, n)
    }

    /// `n` (the new contents of what `p` pointed to) kept, split as it must
    /// be to fit: one entry for its parent, or more.
    fn write_back(&mut self, p: &BlockPtr, n: Node) -> Result<Vec<(Key, BlockPtr)>, FsError> {
        let parts = if let Body::Internal(_) = n.body {
            let mut n = n;
            if n.fits() {
                vec![n]
            } else {
                let r = n.split();
                vec![n, r]
            }
        } else {
            split_to_fit(n)
        };
        let mut out = Vec::with_capacity(parts.len());
        for (i, part) in parts.into_iter().enumerate() {
            let first = part.first_key().unwrap_or_default();
            let ptr = if i == 0 { self.cow(Some(p), part)? } else { self.cow(None, part)? };
            out.push((first, ptr));
        }
        Ok(out)
    }

    /// Takes `key` out, returning the new root and the value it held.
    pub fn tree_delete(
        &mut self,
        root: &BlockPtr,
        tree: u64,
        key: &Key,
    ) -> Result<(BlockPtr, Option<Vec<u8>>), FsError> {
        let (entry, old) = self.delete_at(root, tree, key)?;
        let mut root = match entry {
            Some((_, p)) => p,
            // The tree emptied: an empty leaf is its root.
            None => self.cow(None, Node::leaf(tree))?,
        };
        // A root with one child steps down.
        loop {
            let n = self.load(&root, tree)?;
            match n.body {
                Body::Internal(children) if children.len() == 1 => {
                    self.discard(&root);
                    root = children[0].1;
                }
                _ => return Ok((root, old)),
            }
        }
    }

    fn delete_at(&mut self, p: &BlockPtr, tree: u64, key: &Key) -> Result<Deleted, FsError> {
        let mut n = self.load(p, tree)?;
        let old = match &mut n.body {
            Body::Leaf(items) => match items.binary_search_by(|(k, _)| k.cmp(key)) {
                Ok(i) => Some(items.remove(i).1),
                // Not here: nothing changes, nothing is copied.
                Err(_) => return Ok((Some((n.first_key().unwrap_or_default(), *p)), None)),
            },
            Body::Internal(children) => {
                let i = child_index(children, key);
                let child = children[i].1;
                let (entry, old) = self.delete_at(&child, tree, key)?;
                if old.is_none() {
                    return Ok((Some((n.first_key().unwrap_or_default(), *p)), None));
                }
                match entry {
                    Some(e) => children[i] = e,
                    None => {
                        children.remove(i);
                    }
                }
                old
            }
        };
        if n.count() == 0 {
            self.discard(p);
            return Ok((None, old));
        }
        if let Body::Internal(_) = n.body {
            self.merge_small(&mut n, tree, key)?;
        }
        let first = n.first_key().unwrap_or_default();
        Ok((Some((first, self.cow(Some(p), n)?)), old))
    }

    /// After a delete below `n`: the child `key` went into, if under a
    /// quarter full, joins a neighbour when the two fit in one node.
    fn merge_small(&mut self, n: &mut Node, tree: u64, key: &Key) -> Result<(), FsError> {
        let Body::Internal(children) = &mut n.body else { return Ok(()) };
        if children.len() < 2 {
            return Ok(());
        }
        let i = child_index(children, key);
        let (l, r) = if i + 1 < children.len() { (i, i + 1) } else { (i - 1, i) };
        let (lp, rp) = (children[l].1, children[r].1);
        let (mut left, right) = (self.load(&lp, tree)?, self.load(&rp, tree)?);
        if left.used().min(right.used()) > BLOCK / 4 {
            return Ok(());
        }
        let combined = left.used() + right.used() - crate::node::HEADER;
        if combined > BLOCK {
            return Ok(());
        }
        match (&mut left.body, right.body) {
            (Body::Leaf(a), Body::Leaf(b)) => a.extend(b),
            (Body::Internal(a), Body::Internal(b)) => a.extend(b),
            _ => return Err(FsError::Corrupt("siblings at different levels")),
        }
        self.discard(&rp);
        let first = left.first_key().unwrap_or_default();
        let np = self.cow(Some(&lp), left)?;
        children[l] = (first, np);
        children.remove(r);
        Ok(())
    }

    /// Calls `f` with every item from `from` on, in key order, until it
    /// returns false.
    pub fn tree_scan(
        &mut self,
        root: &BlockPtr,
        tree: u64,
        from: &Key,
        f: &mut dyn FnMut(Key, &[u8]) -> bool,
    ) -> Result<bool, FsError> {
        let n = self.load(root, tree)?;
        match n.body {
            Body::Leaf(items) => {
                for (k, v) in items.iter().filter(|(k, _)| k >= from) {
                    if !f(*k, v) {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Body::Internal(children) => {
                let start = child_index(&children, from);
                for (_, c) in &children[start..] {
                    if !self.tree_scan(c, tree, from, f)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
        }
    }
}
