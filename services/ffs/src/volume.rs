// SPDX-License-Identifier: Apache-2.0
//! A FreallyFS volume: format (§ 15), mount (§ 4, § 14), and the commit
//! (§ 13): the free-space tree brought level with the allocator (each pass
//! may copy free-tree nodes, which allocates; it settles in a few), every
//! dirty node written after its children, a flush, the superblock into its
//! slot, a flush.

use crate::node::{Body, Node};
use crate::ptr::BlockPtr;
use crate::space::Space;
use crate::store::Store;
use crate::superblock::Superblock;
use crate::{BLOCK, FIRST_FREE, FsError, Key, kind, tree};
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

/// The free-space tree's items as written: start → (length, freed generation).
type FreeItems = BTreeMap<u64, (u64, u64)>;

pub struct Volume<S: Store> {
    pub(crate) store: S,
    /// The last committed superblock.
    pub(crate) sb: Superblock,
    /// The generation being built.
    pub(crate) generation: u64,
    pub(crate) space: Space,
    /// This generation's nodes, by block, not yet written.
    pub(crate) dirty: BTreeMap<u64, Node>,
    /// The trees' roots as they stand (this generation's copies once changed).
    pub(crate) root: BlockPtr,
    pub(crate) free_root: BlockPtr,
    pub(crate) ref_root: BlockPtr,
    /// What the free-space tree holds now.
    written_free: FreeItems,
}

impl<S: Store> Volume<S> {
    /// Formats `store` (a whole, empty partition) as generation 1.
    pub fn format(mut store: S, uuid: [u8; 16]) -> Result<Volume<S>, FsError> {
        let blocks = store.blocks();
        if blocks < FIRST_FREE + 64 {
            return Err(FsError::NoSpace);
        }
        for b in 0..FIRST_FREE {
            store.write(b, &[0u8; BLOCK])?;
        }
        let sb = Superblock { block_count: blocks, uuid, next_subvol: 256, ..Default::default() };
        let mut v = Volume {
            store,
            sb,
            generation: 1,
            space: Space::new_volume(blocks),
            dirty: BTreeMap::new(),
            root: BlockPtr::default(),
            free_root: BlockPtr::default(),
            ref_root: BlockPtr::default(),
            written_free: FreeItems::new(),
        };
        v.root = v.cow(None, Node::leaf(tree::ROOT))?;
        v.free_root = v.cow(None, Node::leaf(tree::FREE))?;
        v.ref_root = v.cow(None, Node::leaf(tree::REF))?;
        v.commit()?;
        Ok(v)
    }

    /// Mounts the newest valid generation.
    pub fn mount(mut store: S) -> Result<Volume<S>, FsError> {
        let blocks = store.blocks();
        let mut slot = |i: u64| -> Result<Option<Superblock>, FsError> {
            let mut b = [0u8; BLOCK];
            store.read(i, &mut b)?;
            Ok(Superblock::decode(&b, blocks))
        };
        let (a, b) = (slot(0)?, slot(1)?);
        let sb = Superblock::newest(a, b)?;
        let mut v = Volume {
            store,
            sb,
            generation: sb.generation + 1,
            space: Space::default(),
            dirty: BTreeMap::new(),
            root: sb.root_tree,
            free_root: sb.free_tree,
            ref_root: sb.ref_tree,
            written_free: FreeItems::new(),
        };
        let mut items = FreeItems::new();
        let mut bad = false;
        v.tree_scan(&sb.free_tree, tree::FREE, &Key::default(), &mut |k, val| {
            if k.kind != kind::FREE || val.len() != 16 {
                bad = true;
                return false;
            }
            let len = u64::from_le_bytes(val[..8].try_into().unwrap_or_default());
            let freed = u64::from_le_bytes(val[8..].try_into().unwrap_or_default());
            items.insert(k.object, (len, freed));
            true
        })?;
        if bad {
            return Err(FsError::Corrupt("the free-space tree holds something else"));
        }
        v.space = Space::from_items(items.iter().map(|(&s, &(l, g))| (s, l, g)));
        v.written_free = items;
        v.space.settle(v.generation);
        Ok(v)
    }

    /// The generation last committed.
    pub fn generation(&self) -> u64 {
        self.sb.generation
    }

    pub fn superblock(&self) -> &Superblock {
        &self.sb
    }

    pub fn free_blocks(&self) -> u64 {
        self.space.free_blocks()
    }

    /// Gives the store back (a test's unmount without a commit: a crash).
    pub fn into_store(self) -> S {
        self.store
    }

    pub fn store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    // The root tree's items (part 1's tests, and the subvolume records of part 3).

    pub fn get(&mut self, key: &Key) -> Result<Option<Vec<u8>>, FsError> {
        let root = self.root;
        self.tree_get(&root, tree::ROOT, key)
    }

    pub fn insert(&mut self, key: Key, value: &[u8]) -> Result<(), FsError> {
        let root = self.root;
        self.root = self.tree_insert(&root, tree::ROOT, key, value)?;
        Ok(())
    }

    pub fn delete(&mut self, key: &Key) -> Result<Option<Vec<u8>>, FsError> {
        let root = self.root;
        let (r, old) = self.tree_delete(&root, tree::ROOT, key)?;
        self.root = r;
        Ok(old)
    }

    /// Every item of the root tree, in order.
    pub fn items(&mut self) -> Result<Vec<(Key, Vec<u8>)>, FsError> {
        let root = self.root;
        let mut out = Vec::new();
        self.tree_scan(&root, tree::ROOT, &Key::default(), &mut |k, v| {
            out.push((k, v.to_vec()));
            true
        })?;
        Ok(out)
    }

    /// Brings the free-space tree level with the allocator: until a pass
    /// changes nothing (copying its nodes allocates, freeing their old
    /// blocks frees; each pass copies fewer).
    fn sync_free_tree(&mut self) -> Result<(), FsError> {
        for _ in 0..32 {
            let want: FreeItems = self.space.items().map(|(s, l, g)| (s, (l, g))).collect();
            if want == self.written_free {
                return Ok(());
            }
            let gone: Vec<u64> = self.written_free.keys().filter(|s| !want.contains_key(s)).copied().collect();
            let changed: Vec<(u64, (u64, u64))> =
                want.iter().filter(|(s, e)| self.written_free.get(s) != Some(e)).map(|(&s, &e)| (s, e)).collect();
            self.written_free = want;
            for s in gone {
                let root = self.free_root;
                self.free_root = self.tree_delete(&root, tree::FREE, &Key::new(s, kind::FREE, 0))?.0;
            }
            for (s, (l, g)) in changed {
                let mut val = [0u8; 16];
                val[..8].copy_from_slice(&l.to_le_bytes());
                val[8..].copy_from_slice(&g.to_le_bytes());
                let root = self.free_root;
                self.free_root = self.tree_insert(&root, tree::FREE, Key::new(s, kind::FREE, 0), &val)?;
            }
        }
        Err(FsError::Corrupt("the free-space tree never settled"))
    }

    /// Writes the dirty nodes under `p`, children first, each pointer
    /// carrying its child's checksum; returns `p` as written.
    fn write_tree(&mut self, p: BlockPtr) -> Result<BlockPtr, FsError> {
        if p.generation != self.generation {
            return Ok(p);
        }
        let mut n = self.dirty.remove(&p.block).ok_or(FsError::Corrupt("a pointer to a node never kept"))?;
        if let Body::Internal(children) = &mut n.body {
            for c in children.iter_mut() {
                c.1 = self.write_tree(c.1)?;
            }
        }
        let bytes = n.encode()?;
        self.store.write(p.block, &bytes)?;
        Ok(BlockPtr { block: p.block, generation: p.generation, check: BlockPtr::plain_check(&bytes) })
    }

    /// Commits this generation (§ 13).
    pub fn commit(&mut self) -> Result<(), FsError> {
        self.sync_free_tree()?;
        self.root = self.write_tree(self.root)?;
        self.ref_root = self.write_tree(self.ref_root)?;
        self.free_root = self.write_tree(self.free_root)?;
        if !self.dirty.is_empty() {
            return Err(FsError::Corrupt("nodes kept that no tree reaches"));
        }
        self.store.flush()?;
        let sb = Superblock {
            generation: self.generation,
            root_tree: self.root,
            free_tree: self.free_root,
            ref_tree: self.ref_root,
            used_blocks: self.sb.block_count - FIRST_FREE - self.space.free_blocks(),
            flags: 0,
            ..self.sb
        };
        self.store.write(Superblock::slot(self.generation), &sb.encode())?;
        self.store.flush()?;
        self.sb = sb;
        self.generation += 1;
        self.space.settle(self.generation);
        Ok(())
    }

    /// Checks the committed volume's structure (the start of `ffsck`, § 14):
    /// every node of the three trees reached, read and checked against its
    /// pointer; levels one less below each internal node; keys in order
    /// within nodes and across them; no block reached twice; every block
    /// either reached, free, or one of the first 16, never two of these.
    pub fn check(&mut self) -> Result<CheckReport, FsError> {
        if !self.dirty.is_empty() {
            return Err(FsError::Corrupt("check a committed volume"));
        }
        let mut reached = BTreeSet::new();
        let mut items = 0u64;
        for (p, t) in [(self.sb.root_tree, tree::ROOT), (self.sb.free_tree, tree::FREE), (self.sb.ref_tree, tree::REF)]
        {
            let mut last: Option<Key> = None;
            self.walk(&p, t, None, &mut reached, &mut last, &mut items)?;
        }
        for &b in &reached {
            if b < FIRST_FREE || self.space.is_free(b) {
                return Err(FsError::Corrupt("a block both reached and free (or reserved)"));
            }
        }
        let total = self.sb.block_count;
        if reached.len() as u64 + self.space.free_blocks() + FIRST_FREE != total {
            return Err(FsError::Corrupt("blocks neither reached nor free (a leak)"));
        }
        if self.sb.used_blocks != reached.len() as u64 {
            return Err(FsError::Corrupt("the superblock's used count"));
        }
        Ok(CheckReport { nodes: reached.len() as u64, items })
    }

    fn walk(
        &mut self,
        p: &BlockPtr,
        tree: u64,
        level: Option<u8>,
        reached: &mut BTreeSet<u64>,
        last: &mut Option<Key>,
        items: &mut u64,
    ) -> Result<(), FsError> {
        if !reached.insert(p.block) {
            return Err(FsError::Corrupt("a block reached twice"));
        }
        let n = self.load(p, tree)?;
        if level.is_some_and(|l| l != n.level) {
            return Err(FsError::Corrupt("a child at the wrong level"));
        }
        match n.body {
            Body::Leaf(list) => {
                for (k, _) in list {
                    if last.is_some_and(|l| l >= k) {
                        return Err(FsError::Corrupt("keys out of order across nodes"));
                    }
                    *last = Some(k);
                    *items += 1;
                }
            }
            Body::Internal(children) => {
                for (_, c) in &children {
                    if c.generation > p.generation {
                        return Err(FsError::Corrupt("a child newer than its parent"));
                    }
                    self.walk(c, tree, Some(n.level - 1), reached, last, items)?;
                }
            }
        }
        Ok(())
    }
}

/// What [`Volume::check`] found: the nodes reached and the items in them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckReport {
    pub nodes: u64,
    pub items: u64,
}
