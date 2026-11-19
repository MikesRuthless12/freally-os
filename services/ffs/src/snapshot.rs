// SPDX-License-Identifier: Apache-2.0
//! Snapshots and reference counts (§ 9). A snapshot is a read-only
//! subvolume sharing its source's tree: its record points at the same root,
//! which gains a reference. Only subvolume trees share. A shared node is
//! never changed: before it's changed it's *owned* (Rodeh's scheme): its
//! copy's children each gain a reference (a leaf's: every data block its
//! extents name) and the node loses one; so a child is never freed while
//! another tree's node still points to it. A block with no item in the
//! reference tree has one reference; at its last it's freed.

use crate::extent::Extent;
use crate::node::{Body, Node};
use crate::ptr::BlockPtr;
use crate::store::Store;
use crate::subvol::Open;
use crate::volume::Volume;
use crate::{FsError, Key, kind, tree};
use alloc::vec::Vec;

/// A snapshot's record flag: read-only.
pub const READ_ONLY: u8 = 1;

/// Whether a tree's blocks can be shared: subvolume trees only.
pub(crate) fn shares(tree: u64) -> bool {
    tree >= crate::subvol::SYSTEM
}

/// The data blocks a leaf's extents name.
fn data_blocks(n: &Node) -> Result<Vec<u64>, FsError> {
    let mut out = Vec::new();
    if let Body::Leaf(items) = &n.body {
        for (k, v) in items {
            if k.kind == kind::EXTENT {
                let e = Extent::decode(v)?;
                out.extend((0..e.count()).map(|i| e.block + i));
            }
        }
    }
    Ok(out)
}

impl<S: Store> Volume<S> {
    fn is_dirty(&self, p: &BlockPtr) -> bool {
        p.generation == self.generation && self.dirty.contains_key(&p.block)
    }

    /// The references `block` has (1 when the reference tree has no item).
    pub fn refs(&mut self, block: u64) -> Result<u32, FsError> {
        let root = self.ref_root;
        Ok(match self.tree_get(&root, tree::REF, &Key::new(block, kind::REF, 0))? {
            Some(v) => u32::from_le_bytes(
                v.get(..4).and_then(|b| b.try_into().ok()).ok_or(FsError::Corrupt("a reference count"))?,
            ),
            None => 1,
        })
    }

    pub(crate) fn set_refs(&mut self, block: u64, n: u32) -> Result<(), FsError> {
        let root = self.ref_root;
        let key = Key::new(block, kind::REF, 0);
        self.ref_root = if n <= 1 {
            self.tree_delete(&root, tree::REF, &key)?.0
        } else {
            self.tree_insert(&root, tree::REF, key, &n.to_le_bytes())?
        };
        Ok(())
    }

    pub(crate) fn inc_ref(&mut self, block: u64) -> Result<(), FsError> {
        let n = self.refs(block)?;
        self.set_refs(block, n + 1)
    }

    /// Whether `p`, in `tree`, is shared with another tree.
    pub(crate) fn is_shared(&mut self, p: &BlockPtr, tree: u64) -> Result<bool, FsError> {
        Ok(shares(tree) && !self.is_dirty(p) && self.refs(p.block)? > 1)
    }

    /// Before `p` (holding `original`) changes: if it's shared, its copy's
    /// children gain a reference and it loses one; the copy then takes a
    /// new block without freeing the old (the other tree's).
    pub(crate) fn own(&mut self, p: &BlockPtr, original: &Node, tree: u64) -> Result<(), FsError> {
        if !self.is_shared(p, tree)? {
            return Ok(());
        }
        match &original.body {
            Body::Internal(children) => {
                for (_, c) in children {
                    self.inc_ref(c.block)?;
                }
            }
            Body::Leaf(_) => {
                for b in data_blocks(original)? {
                    self.inc_ref(b)?;
                }
            }
        }
        let n = self.refs(p.block)?;
        self.set_refs(p.block, n - 1)?;
        self.shared_copied.insert(p.block);
        Ok(())
    }

    /// A data block one file no longer names: this generation's goes at
    /// once; a shared one loses a reference; else it's freed.
    pub(crate) fn release_data(&mut self, block: u64) -> Result<(), FsError> {
        if self.data.remove(&block).is_some() {
            self.space.free(block, 0);
            return Ok(());
        }
        match self.refs(block)? {
            n if n > 1 => self.set_refs(block, n - 1),
            _ => {
                self.space.free(block, self.generation);
                Ok(())
            }
        }
    }

    /// A whole tree under `p` loses its reference (a subvolume deleted):
    /// shared parts lose one; the rest is freed, data and all.
    pub(crate) fn release_tree(&mut self, p: &BlockPtr, tree: u64) -> Result<(), FsError> {
        if !self.is_dirty(p) && self.refs(p.block)? > 1 {
            let n = self.refs(p.block)?;
            return self.set_refs(p.block, n - 1);
        }
        let n = self.load(p, tree)?;
        match &n.body {
            Body::Internal(children) => {
                for (_, c) in children.clone() {
                    self.release_tree(&c, tree)?;
                }
            }
            Body::Leaf(_) => {
                for b in data_blocks(&n)? {
                    self.release_data(b)?;
                }
            }
        }
        if self.dirty.remove(&p.block).is_some() && p.generation == self.generation {
            self.space.free(p.block, 0);
        } else {
            self.space.free(p.block, self.generation);
        }
        Ok(())
    }

    /// A read-only snapshot of subvolume `src` called `name`: what `src`
    /// holds now (committed first), sharing every block. Its id.
    pub fn snapshot(&mut self, src: u64, name: &str) -> Result<u64, FsError> {
        self.commit()?;
        let mut rec = self.subvol(src)?.rec.clone();
        let id = self.next_subvol_id();
        self.inc_ref(rec.root.block)?;
        rec.flags |= READ_ONLY;
        rec.snapshot_of = src;
        rec.created_unix = self.now / 1_000_000_000;
        rec.name = alloc::string::String::from(name);
        self.subvols.insert(id, Open { rec, changed: true });
        Ok(id)
    }
}
