// SPDX-License-Identifier: Apache-2.0
//! `ffsck` (§ 14): checks a committed volume and reports every finding by
//! its block or key; repairs nothing (v1). The host tool (`tools/ffsck`)
//! and the OS at an unclean mount ([`Volume::mount_checked`]) run it.
//!
//! 1. The plain trees (root, free-space, reference): every pointer's block
//!    read and checked; levels; keys in order within nodes and between
//!    separators; each node reached once.
//! 2. Every subvolume's tree from its record, shared nodes visited once and
//!    the pointers to each block counted (records, parent nodes, extents).
//! 3. Reference counts: each block's count of pointers equals its count in
//!    the reference tree (1 without an item); no item for a block nothing
//!    reaches.
//! 4. Free space: inside the volume, never overlapping, never covering a
//!    reached block; every block neither reached nor free is a leak.
//! 5. Each subvolume's files: directory entries' hashes and children, link
//!    counts, directory sizes and parents, orphans, extents inside their
//!    file and never overlapping, every data block against its check.

use crate::dir::name_hash;
use crate::extent::Extent;
use crate::inode::{FileKind, Inode, ROOT_INO};
use crate::node::{Body, MAX_LEVEL};
use crate::ptr::BlockPtr;
use crate::store::Store;
use crate::subvol::SubvolRecord;
use crate::volume::Volume;
use crate::{BLOCK, FIRST_FREE, FsError, Key, kind, tree};
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// One thing wrong: where (a block, or a subvolume and key) and what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub block: Option<u64>,
    pub what: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub findings: Vec<Finding>,
    /// Nodes and data blocks reached, and items read.
    pub nodes: u64,
    pub data: u64,
    pub items: u64,
    /// Encrypted subvolumes checked without their key: their insides
    /// weren't read, so neither reference counts nor leaks were judged.
    pub locked: Vec<u64>,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    fn add(&mut self, block: Option<u64>, what: String) {
        self.findings.push(Finding { block, what });
    }

    /// Whether any finding mentions `text`.
    pub fn says(&self, text: &str) -> bool {
        self.findings.iter().any(|f| f.what.contains(text))
    }
}

/// A tree's items, in order.
type Items = Vec<(Key, Vec<u8>)>;

/// What a walk gathers.
#[derive(Default)]
struct Walk {
    /// Pointers to each node or data block, counted from distinct parents.
    ptrs: BTreeMap<u64, u32>,
    /// Nodes already read (a shared node's children are counted once).
    seen: BTreeSet<u64>,
    /// Plain trees' nodes (never shared).
    plain: BTreeSet<u64>,
}

impl<S: Store> Volume<S> {
    /// Mounts, and runs `ffsck` when the volume wasn't unmounted cleanly.
    pub fn mount_checked(store: S) -> Result<(Volume<S>, Option<Report>), FsError> {
        let mut v = Volume::mount(store)?;
        let report = if v.was_clean() { None } else { Some(v.fsck()?) };
        Ok((v, report))
    }

    /// Checks the committed volume (§ 14).
    pub fn fsck(&mut self) -> Result<Report, FsError> {
        if !self.dirty.is_empty() || !self.data.is_empty() {
            return Err(FsError::Corrupt("ffsck checks a committed volume"));
        }
        let mut r = Report::default();
        let mut w = Walk::default();
        let sb = self.sb;
        // 1. The plain trees, their items kept.
        let mut root_items = Vec::new();
        let mut free_items = Vec::new();
        let mut ref_items = Vec::new();
        for (p, t, out) in [
            (sb.root_tree, tree::ROOT, &mut root_items),
            (sb.free_tree, tree::FREE, &mut free_items),
            (sb.ref_tree, tree::REF, &mut ref_items),
        ] {
            self.walk(&p, t, None, (None, None), &mut w, &mut r, true, out);
        }
        // 2. Every subvolume.
        let mut subvols = Vec::new();
        for (k, v) in &root_items {
            if k.kind != kind::SUBVOL {
                r.add(None, format!("the root tree holds item {k:?}, which isn't a subvolume"));
                continue;
            }
            match SubvolRecord::decode(v) {
                Ok(rec) => subvols.push((k.object, rec)),
                Err(e) => r.add(None, format!("subvolume {}'s record: {e:?}", k.object)),
            }
        }
        let mut sv_items: Vec<(u64, Items)> = Vec::new();
        for (id, rec) in &subvols {
            if self.is_locked(*id)? {
                r.locked.push(*id);
                continue;
            }
            *w.ptrs.entry(rec.root.block).or_default() += 1;
            let mut items = Vec::new();
            self.walk(&rec.root, *id, None, (None, None), &mut w, &mut r, false, &mut items);
            // Its items whole, shared nodes and all (the walk reads each node once).
            let mut all = Vec::new();
            let root = rec.root;
            if self
                .tree_scan(&root, *id, &Key::default(), &mut |k, v| {
                    all.push((k, v.to_vec()));
                    true
                })
                .is_ok()
            {
                sv_items.push((*id, all));
            }
        }
        // 3. Reference counts (only with every subvolume read).
        let whole = r.locked.is_empty();
        let mut refs = BTreeMap::new();
        for (k, v) in &ref_items {
            let n = v.get(..4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes).unwrap_or(0);
            if k.kind != kind::REF || n < 2 {
                r.add(Some(k.object), format!("a reference item of {n} (an item holds 2 or more)"));
            }
            refs.insert(k.object, n);
        }
        for (&b, &n) in &w.ptrs {
            let want = refs.get(&b).copied().unwrap_or(1);
            if whole && n != want && !w.plain.contains(&b) {
                r.add(Some(b), format!("block {b} is pointed to {n} time(s) but its reference count is {want}"));
            }
        }
        for &b in refs.keys() {
            if whole && !w.ptrs.contains_key(&b) {
                r.add(Some(b), format!("a reference count for block {b}, which nothing reaches"));
            }
        }
        // 4. Free space.
        let total = sb.block_count;
        let mut free = BTreeMap::new();
        for (k, v) in &free_items {
            let (len, _) = match (v.get(..8), v.get(8..16)) {
                (Some(a), Some(b)) => (
                    u64::from_le_bytes(a.try_into().unwrap_or_default()),
                    u64::from_le_bytes(b.try_into().unwrap_or_default()),
                ),
                _ => {
                    r.add(None, format!("a free-space item at {} of the wrong length", k.object));
                    continue;
                }
            };
            if k.object < FIRST_FREE || len == 0 || k.object.saturating_add(len) > total {
                r.add(Some(k.object), format!("free extent {}+{len} outside the volume", k.object));
            }
            free.insert(k.object, len);
        }
        let mut last_end = 0u64;
        for (&s, &l) in &free {
            if s < last_end {
                r.add(Some(s), format!("free extents overlap at block {s}"));
            }
            last_end = last_end.max(s + l);
        }
        let is_free = |b: u64| free.range(..=b).next_back().is_some_and(|(&s, &l)| b < s + l);
        let reached: BTreeSet<u64> = w.ptrs.keys().chain(w.plain.iter()).copied().collect();
        for &b in &reached {
            if b < FIRST_FREE || b >= total {
                r.add(Some(b), format!("block {b} is used but outside the volume's free area"));
            } else if is_free(b) {
                r.add(Some(b), format!("block {b} is both used and free"));
            }
        }
        let mut leak_start: Option<u64> = None;
        for b in FIRST_FREE..=total {
            let leaked = whole && b < total && !reached.contains(&b) && !is_free(b);
            match (leaked, leak_start) {
                (true, None) => leak_start = Some(b),
                (false, Some(s)) => {
                    r.add(Some(s), format!("blocks {s} to {} are neither used nor free (a leak)", b - 1));
                    leak_start = None;
                }
                _ => {}
            }
        }
        if whole && sb.used_blocks != reached.len() as u64 {
            r.add(None, format!("the superblock says {} blocks are used; {} are", sb.used_blocks, reached.len()));
        }
        r.nodes = (w.seen.len() + w.plain.len()) as u64;
        r.data = w.ptrs.keys().filter(|b| !w.seen.contains(b) && !w.plain.contains(b)).count() as u64;
        // 5. Each subvolume's files.
        for (id, items) in sv_items {
            self.check_files(id, &items, &mut r);
        }
        Ok(r)
    }

    /// Reads the node under `p` (each once) and its subtree, counting
    /// pointers; keys must lie in `[lo, hi)`.
    #[allow(clippy::too_many_arguments)]
    fn walk(
        &mut self,
        p: &BlockPtr,
        t: u64,
        level: Option<u8>,
        (lo, hi): (Option<Key>, Option<Key>),
        w: &mut Walk,
        r: &mut Report,
        plain: bool,
        items: &mut Vec<(Key, Vec<u8>)>,
    ) {
        if plain {
            if !w.plain.insert(p.block) {
                r.add(Some(p.block), format!("block {} is reached twice in a tree that never shares", p.block));
                return;
            }
        } else if !w.seen.insert(p.block) {
            return;
        }
        let n = match self.load(p, t) {
            Ok(n) => n,
            Err(e) => {
                r.add(Some(p.block), format!("block {} (tree {t}) can't be trusted: {e:?}", p.block));
                return;
            }
        };
        if level.is_some_and(|l| l != n.level) || n.level > MAX_LEVEL {
            r.add(Some(p.block), format!("block {} is at level {}, not where its parent says", p.block, n.level));
        }
        let in_bounds = |k: &Key| lo.is_none_or(|l| *k >= l) && hi.is_none_or(|h| *k < h);
        match n.body {
            Body::Leaf(list) => {
                for (k, v) in list {
                    r.items += 1;
                    if !in_bounds(&k) {
                        r.add(Some(p.block), format!("key {k:?} in block {} is outside its parent's range", p.block));
                    }
                    if !plain
                        && k.kind == kind::EXTENT
                        && let Ok(e) = Extent::decode(&v)
                    {
                        for i in 0..e.count() {
                            *w.ptrs.entry(e.block + i).or_default() += 1;
                        }
                    }
                    items.push((k, v));
                }
            }
            Body::Internal(children) => {
                for (i, (k, c)) in children.iter().enumerate() {
                    if !in_bounds(k) && !(i == 0 && lo.is_some_and(|l| *k <= l)) {
                        r.add(
                            Some(p.block),
                            format!("separator {k:?} in block {} is outside its parent's range", p.block),
                        );
                    }
                    if c.generation > p.generation {
                        r.add(Some(c.block), format!("block {} is newer than its parent {}", c.block, p.block));
                    }
                    if !plain {
                        *w.ptrs.entry(c.block).or_default() += 1;
                    }
                    let child_lo = if i == 0 { lo } else { Some(*k) };
                    let child_hi = children.get(i + 1).map(|n| n.0).or(hi);
                    self.walk(c, t, Some(n.level.saturating_sub(1)), (child_lo, child_hi), w, r, plain, items);
                }
            }
        }
    }

    /// Step 5 for subvolume `sv`, from its items.
    fn check_files(&mut self, sv: u64, items: &[(Key, Vec<u8>)], r: &mut Report) {
        let mut inodes: BTreeMap<u64, Inode> = BTreeMap::new();
        let mut named: BTreeMap<u64, u32> = BTreeMap::new();
        let mut entries_in: BTreeMap<u64, u64> = BTreeMap::new();
        let mut subdirs: BTreeMap<u64, u32> = BTreeMap::new();
        let mut dir_children: Vec<(u64, u64, FileKind)> = Vec::new();
        let mut extents: Vec<(u64, u64, Extent)> = Vec::new();
        for (k, v) in items {
            match k.kind {
                kind::INODE => match Inode::decode(v) {
                    Ok(i) => {
                        inodes.insert(k.object, i);
                    }
                    Err(e) => r.add(None, format!("subvolume {sv}: inode {}: {e:?}", k.object)),
                },
                kind::DIR => {
                    let mut rest: &[u8] = v;
                    while !rest.is_empty() {
                        if rest.len() < 11 {
                            r.add(None, format!("subvolume {sv}: directory {}: an entry cut short", k.object));
                            break;
                        }
                        let child = u64::from_le_bytes(rest[..8].try_into().unwrap_or_default());
                        let kd = rest[8];
                        let n = u16::from_le_bytes([rest[9], rest[10]]) as usize;
                        let Some(name) = rest.get(11..11 + n).and_then(|b| core::str::from_utf8(b).ok()) else {
                            r.add(
                                None,
                                format!("subvolume {sv}: directory {}: a name cut short or not UTF-8", k.object),
                            );
                            break;
                        };
                        if name_hash(name) != k.offset {
                            r.add(
                                None,
                                format!("subvolume {sv}: \"{name}\" in directory {} under the wrong hash", k.object),
                            );
                        }
                        let Ok(kind) = FileKind::from_u8(kd) else {
                            r.add(None, format!("subvolume {sv}: \"{name}\" of no known kind"));
                            break;
                        };
                        *named.entry(child).or_default() += 1;
                        *entries_in.entry(k.object).or_default() += 1;
                        if kind == FileKind::Dir {
                            *subdirs.entry(k.object).or_default() += 1;
                        }
                        dir_children.push((k.object, child, kind));
                        rest = &rest[11 + n..];
                    }
                }
                kind::EXTENT => match Extent::decode(v) {
                    Ok(e) => extents.push((k.object, k.offset, e)),
                    Err(e) => {
                        r.add(None, format!("subvolume {sv}: inode {}'s extent at {}: {e:?}", k.object, k.offset))
                    }
                },
                _ => r.add(None, format!("subvolume {sv}: an item of no known kind: {k:?}")),
            }
        }
        for &(dir, child, kd) in &dir_children {
            match inodes.get(&child) {
                None => r.add(None, format!("subvolume {sv}: directory {dir} names inode {child}, which isn't there")),
                Some(i) if i.kind != kd => r.add(
                    None,
                    format!("subvolume {sv}: directory {dir} says inode {child} is a {kd:?}; it's a {:?}", i.kind),
                ),
                Some(i) if kd == FileKind::Dir && i.parent != dir => r.add(
                    None,
                    format!("subvolume {sv}: directory {child}'s parent is {}, but {dir} lists it", i.parent),
                ),
                _ => {}
            }
            if !inodes.get(&dir).is_some_and(|i| i.kind == FileKind::Dir) {
                r.add(None, format!("subvolume {sv}: entries under {dir}, which isn't a directory"));
            }
        }
        for (&ino, i) in &inodes {
            let links = named.get(&ino).copied().unwrap_or(0);
            match i.kind {
                FileKind::File if links != i.nlink => {
                    r.add(None, format!("subvolume {sv}: file {ino} has {links} name(s) but nlink {}", i.nlink))
                }
                FileKind::Dir => {
                    let want = 2 + subdirs.get(&ino).copied().unwrap_or(0);
                    if i.nlink != want {
                        r.add(None, format!("subvolume {sv}: directory {ino} has nlink {}, not {want}", i.nlink));
                    }
                    let n = entries_in.get(&ino).copied().unwrap_or(0);
                    if i.size != n {
                        r.add(None, format!("subvolume {sv}: directory {ino} says {} entries; it has {n}", i.size));
                    }
                }
                _ => {}
            }
            if ino == ROOT_INO {
                if i.parent != ROOT_INO {
                    r.add(None, format!("subvolume {sv}: the root directory's parent is {}", i.parent));
                }
            } else if links == 0 {
                r.add(None, format!("subvolume {sv}: inode {ino} is named nowhere (an orphan)"));
            }
        }
        let mut last: Option<(u64, u64)> = None;
        for (ino, off, e) in &extents {
            let size = inodes.get(ino).map(|i| i.size);
            match size {
                None => r.add(Some(e.block), format!("subvolume {sv}: an extent of inode {ino}, which isn't there")),
                Some(s) if *off >= s.max(1).next_multiple_of(BLOCK as u64) || *off % BLOCK as u64 != 0 => {
                    r.add(Some(e.block), format!("subvolume {sv}: inode {ino}'s extent at {off} is past its size {s}"))
                }
                _ => {}
            }
            if let Some((li, lend)) = last
                && li == *ino
                && *off < lend
            {
                r.add(Some(e.block), format!("subvolume {sv}: inode {ino}'s extents overlap at {off}"));
            }
            last = Some((*ino, off + e.count() * BLOCK as u64));
            for (i, c) in e.checks.iter().enumerate() {
                let b = e.block + i as u64;
                if self.read_data(sv, b, c).is_err() {
                    r.add(Some(b), format!("subvolume {sv}: data block {b} of inode {ino} fails its check"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dir::name_hash;
    use crate::store::MemStore;
    use crate::subvol::SYSTEM;
    use crate::superblock::Superblock;

    /// A good volume: a home with files and a directory, a snapshot, more
    /// changes after it; committed and clean.
    fn good() -> (Volume<MemStore>, u64, u64) {
        let mut v = Volume::format(MemStore::new(4096), [3; 16]).unwrap();
        let home = v.create_subvol("home", [1; 16]).unwrap();
        let docs = v.create(home, ROOT_INO, "Documents", FileKind::Dir).unwrap();
        for i in 0..12u64 {
            let f = v.create(home, if i % 2 == 0 { ROOT_INO } else { docs }, &format!("f{i}"), FileKind::File).unwrap();
            v.write(home, f, i * 1000, &alloc::vec![i as u8 + 1; 9000]).unwrap();
        }
        v.snapshot(home, "snap").unwrap();
        let f0 = v.lookup(home, ROOT_INO, "f0").unwrap().unwrap().child;
        v.write(home, f0, 100, &[0xEE; 5000]).unwrap();
        v.commit().unwrap();
        let r = v.fsck().unwrap();
        assert!(r.is_clean(), "{:#?}", r.findings);
        (v, home, f0)
    }

    fn after(mut v: Volume<MemStore>) -> Report {
        v.commit().unwrap();
        Volume::mount(v.into_store()).unwrap().fsck().unwrap()
    }

    /// Some data block of file `ino` in `sv`.
    fn a_data_block(v: &mut Volume<MemStore>, sv: u64, ino: u64) -> (Key, Extent) {
        let root = v.sv_root(sv).unwrap();
        let mut found = None;
        v.tree_scan(&root, sv, &Key::new(ino, kind::EXTENT, 0), &mut |k, val| {
            if k.object == ino && k.kind == kind::EXTENT {
                found = Some((k, Extent::decode(val).unwrap()));
            }
            false
        })
        .unwrap();
        found.unwrap()
    }

    #[test]
    fn every_planted_inconsistency_is_found() {
        let says = |r: &Report, what: &str, plant: &str| {
            assert!(r.says(what), "{plant}: ffsck didn't say \"{what}\": {:#?}", r.findings);
        };
        // A flipped byte in a node.
        let (v, ..) = good();
        let leaf = v.superblock().free_tree.block;
        let mut s = v.into_store();
        s.flip(leaf, 300);
        let mut m = Volume::mount(s);
        // The free tree is read at mount: the damage stops it, said.
        assert!(matches!(m.as_mut().map(|_| ()), Err(FsError::Integrity(_))), "a flipped free-tree node");
        let (v, home, _) = good();
        let root = {
            let mut v = v;
            let p = v.sv_root(home).unwrap();
            (v, p)
        };
        let (v, p) = root;
        let mut s = v.into_store();
        s.flip(p.block, 300);
        says(&Volume::mount(s).unwrap().fsck().unwrap(), "can't be trusted", "a flipped subvolume node");
        // A leak: a block taken and never used.
        let (mut v, ..) = good();
        let g = v.generation;
        v.space.alloc(g).unwrap();
        says(&after(v), "a leak", "a leaked block");
        // A block both used and free.
        let (mut v, home, f0) = good();
        let (_, e) = a_data_block(&mut v, home, f0);
        v.space.free(e.block + e.count() - 1, 0);
        says(&after(v), "both used and free", "a used block freed");
        // A reference count too high, and one for a block nothing reaches.
        let (mut v, home, _) = good();
        let p = v.sv_root(home).unwrap();
        v.inc_ref(p.block).unwrap();
        says(&after(v), "its reference count is", "a reference count too high");
        let (mut v, ..) = good();
        // The highest free block: commits take the lowest, so it stays free.
        let spare = v.space.items().last().map(|(s, l, _)| s + l - 1).unwrap();
        v.set_refs(spare, 2).unwrap();
        says(&after(v), "nothing reaches", "a reference for a free block");
        // An entry naming no inode; a wrong link count; an orphan; a wrong hash.
        let (mut v, ..) = good();
        let mut entry = alloc::vec::Vec::new();
        entry.extend_from_slice(&9999u64.to_le_bytes());
        entry.push(FileKind::File as u8);
        entry.extend_from_slice(&5u16.to_le_bytes());
        entry.extend_from_slice(b"ghost");
        v.sv_insert(SYSTEM, Key::new(ROOT_INO, kind::DIR, name_hash("ghost")), &entry).unwrap();
        says(&after(v), "names inode 9999, which isn't there", "an entry naming no inode");
        let (mut v, home, f0) = good();
        let mut i = v.stat(home, f0).unwrap();
        i.nlink += 1;
        v.put_inode(home, f0, &i).unwrap();
        says(&after(v), "but nlink 2", "a link count");
        let (mut v, home, _) = good();
        v.put_inode(home, 5000, &Inode::new(FileKind::File, ROOT_INO, 0)).unwrap();
        says(&after(v), "an orphan", "an orphan");
        let (mut v, ..) = good();
        let mut e2 = alloc::vec::Vec::new();
        e2.extend_from_slice(&257u64.to_le_bytes());
        e2.push(FileKind::Dir as u8);
        e2.extend_from_slice(&1u16.to_le_bytes());
        e2.extend_from_slice(b"x");
        v.sv_insert(SYSTEM, Key::new(ROOT_INO, kind::DIR, 12345), &e2).unwrap();
        says(&after(v), "under the wrong hash", "a wrong hash");
        // An extent past its file's end; a flipped data block.
        let (mut v, home, f0) = good();
        let (_, e) = a_data_block(&mut v, home, f0);
        v.sv_insert(home, Key::new(f0, kind::EXTENT, 1 << 30), &e.encode()).unwrap();
        says(&after(v), "past its size", "an extent past the end");
        let (mut v, home, f0) = good();
        let (_, e) = a_data_block(&mut v, home, f0);
        let mut s = v.into_store();
        s.flip(e.block, 7);
        says(&Volume::mount(s).unwrap().fsck().unwrap(), "fails its check", "a flipped data block");
        // Keys out of order inside a node (written with its checksum).
        let (mut v, home, f0) = good();
        v.write(home, f0, 0, b"dirty").unwrap();
        let leaf = v.dirty.values_mut().find(|n| n.tree == home && n.level == 0 && n.count() >= 2).unwrap();
        if let Body::Leaf(items) = &mut leaf.body {
            items.swap(0, 1);
        }
        says(&after(v), "keys out of order", "keys out of order");
        // The superblock's used count.
        let (v, ..) = good();
        let mut sb: Superblock = *v.superblock();
        sb.used_blocks += 1;
        let mut s = v.into_store();
        crate::store::Store::write(&mut s, Superblock::slot(sb.generation), &sb.encode()).unwrap();
        says(&Volume::mount(s).unwrap().fsck().unwrap(), "the superblock says", "the used count");
    }
}
