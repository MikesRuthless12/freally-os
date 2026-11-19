// SPDX-License-Identifier: Apache-2.0
//! Subvolumes (§ 7): each a tree of its own, its record in the root tree
//! under `(id, SUBVOL, 0)`. Subvolume 256 is the system's (the account
//! records); each account's home is one more (part 3, TASK-096; encrypted,
//! TASK-098). A subvolume's tree root and next inode change in memory as
//! its files do; the commit writes its tree and then its record.

use crate::ptr::BlockPtr;
use crate::store::Store;
use crate::volume::Volume;
use crate::{FsError, Key, kind, tree};
use alloc::string::String;

/// The system subvolume.
pub const SYSTEM: u64 = 256;

/// A subvolume's record (200 bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubvolRecord {
    pub root: BlockPtr,
    /// 0 plain, 1 encrypted.
    pub kind: u8,
    /// Bit 0: read-only (a snapshot).
    pub flags: u8,
    pub next_inode: u64,
    pub snapshot_of: u64,
    pub created_unix: u64,
    pub owner: [u8; 16],
    pub key_check: [u8; 48],
    pub name: String,
}

impl Default for SubvolRecord {
    fn default() -> SubvolRecord {
        SubvolRecord {
            root: BlockPtr::default(),
            kind: 0,
            flags: 0,
            next_inode: 0,
            snapshot_of: 0,
            created_unix: 0,
            owner: [0; 16],
            key_check: [0; 48],
            name: String::new(),
        }
    }
}

impl SubvolRecord {
    pub const LEN: usize = 200;

    pub fn encode(&self) -> [u8; 200] {
        let mut b = [0u8; 200];
        b[..32].copy_from_slice(&self.root.bytes());
        b[32] = self.kind;
        b[33] = self.flags;
        b[40..48].copy_from_slice(&self.next_inode.to_le_bytes());
        b[48..56].copy_from_slice(&self.snapshot_of.to_le_bytes());
        b[56..64].copy_from_slice(&self.created_unix.to_le_bytes());
        b[64..80].copy_from_slice(&self.owner);
        b[80..128].copy_from_slice(&self.key_check);
        let name = &self.name.as_bytes()[..self.name.len().min(64)];
        b[128] = name.len() as u8;
        b[129..129 + name.len()].copy_from_slice(name);
        b
    }

    pub fn decode(b: &[u8]) -> Result<SubvolRecord, FsError> {
        if b.len() != Self::LEN {
            return Err(FsError::Corrupt("a subvolume record's length"));
        }
        let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap_or_default());
        let n = (b[128] as usize).min(64);
        let mut owner = [0u8; 16];
        owner.copy_from_slice(&b[64..80]);
        let mut key_check = [0u8; 48];
        key_check.copy_from_slice(&b[80..128]);
        Ok(SubvolRecord {
            root: BlockPtr::from_bytes(&b[..32]),
            kind: b[32],
            flags: b[33],
            next_inode: u64_at(40),
            snapshot_of: u64_at(48),
            created_unix: u64_at(56),
            owner,
            key_check,
            name: String::from(
                core::str::from_utf8(&b[129..129 + n]).map_err(|_| FsError::Corrupt("a subvolume's name"))?,
            ),
        })
    }
}

/// A subvolume as this generation has it: its record, and whether it
/// changed (its tree, its next inode) since the last commit.
#[derive(Clone, Debug)]
pub(crate) struct Open {
    pub rec: SubvolRecord,
    pub changed: bool,
}

impl<S: Store> Volume<S> {
    /// Subvolume `id`'s state, read from its record the first time.
    pub(crate) fn subvol(&mut self, id: u64) -> Result<&mut Open, FsError> {
        if !self.subvols.contains_key(&id) {
            let root = self.root;
            let raw = self
                .tree_get(&root, tree::ROOT, &Key::new(id, kind::SUBVOL, 0))?
                .ok_or(FsError::Corrupt("no such subvolume"))?;
            self.subvols.insert(id, Open { rec: SubvolRecord::decode(&raw)?, changed: false });
        }
        self.subvols.get_mut(&id).ok_or(FsError::Corrupt("no such subvolume"))
    }

    /// Makes subvolume `id` with an empty tree (its root directory is the caller's).
    pub(crate) fn make_subvol(&mut self, id: u64, name: &str) -> Result<(), FsError> {
        let root = self.cow(None, crate::node::Node::leaf(id))?;
        let rec =
            SubvolRecord { root, next_inode: crate::inode::ROOT_INO, name: String::from(name), ..Default::default() };
        self.subvols.insert(id, Open { rec, changed: true });
        Ok(())
    }

    /// The subvolume's current tree root.
    pub(crate) fn sv_root(&mut self, id: u64) -> Result<BlockPtr, FsError> {
        Ok(self.subvol(id)?.rec.root)
    }

    pub(crate) fn sv_get(&mut self, id: u64, key: &Key) -> Result<Option<alloc::vec::Vec<u8>>, FsError> {
        let root = self.sv_root(id)?;
        self.tree_get(&root, id, key)
    }

    /// Refuses a change to a read-only subvolume (a snapshot). Every
    /// operation that changes a subvolume asks first, before it takes a
    /// block or an inode number.
    pub(crate) fn writable(&mut self, id: u64) -> Result<(), FsError> {
        if self.subvol(id)?.rec.flags & crate::snapshot::READ_ONLY != 0 {
            return Err(FsError::ReadOnly);
        }
        Ok(())
    }

    pub(crate) fn sv_insert(&mut self, id: u64, key: Key, value: &[u8]) -> Result<(), FsError> {
        self.writable(id)?;
        let root = self.sv_root(id)?;
        let r = self.tree_insert(&root, id, key, value)?;
        let s = self.subvol(id)?;
        (s.rec.root, s.changed) = (r, true);
        Ok(())
    }

    pub(crate) fn sv_delete(&mut self, id: u64, key: &Key) -> Result<Option<alloc::vec::Vec<u8>>, FsError> {
        self.writable(id)?;
        let root = self.sv_root(id)?;
        let (r, old) = self.tree_delete(&root, id, key)?;
        let s = self.subvol(id)?;
        (s.rec.root, s.changed) = (r, true);
        Ok(old)
    }

    /// The next subvolume id, taken (the superblock keeps the counter).
    pub(crate) fn next_subvol_id(&mut self) -> u64 {
        let id = self.sb.next_subvol.max(SYSTEM + 1);
        self.sb.next_subvol = id + 1;
        id
    }

    /// A new subvolume called `name` (owned by `owner`, an account's id),
    /// with its root directory (inode 256). Its id.
    pub fn create_subvol(&mut self, name: &str, owner: [u8; 16]) -> Result<u64, FsError> {
        let id = self.next_subvol_id();
        self.make_subvol(id, name)?;
        let ino = self.take_ino(id)?;
        let mut root_dir = crate::inode::Inode::new(crate::inode::FileKind::Dir, ino, self.now);
        root_dir.nlink = 2;
        self.put_inode(id, ino, &root_dir)?;
        let created = self.now / 1_000_000_000;
        let s = self.subvol(id)?;
        s.rec.owner = owner;
        s.rec.created_unix = created;
        Ok(id)
    }

    /// Every subvolume: its id and record.
    pub fn subvols(&mut self) -> Result<alloc::vec::Vec<(u64, SubvolRecord)>, FsError> {
        let root = self.root;
        let mut raw = alloc::vec::Vec::new();
        self.tree_scan(&root, tree::ROOT, &Key::new(0, kind::SUBVOL, 0), &mut |k, v| {
            if k.kind == kind::SUBVOL {
                raw.push((k.object, v.to_vec()));
            }
            true
        })?;
        let mut out = alloc::vec::Vec::new();
        for (id, v) in raw {
            // This generation's state when it changed.
            let rec = match self.subvols.get(&id) {
                Some(o) => o.rec.clone(),
                None => SubvolRecord::decode(&v)?,
            };
            out.push((id, rec));
        }
        for (&id, o) in &self.subvols {
            if !out.iter().any(|(i, _)| *i == id) {
                out.push((id, o.rec.clone()));
            }
        }
        out.sort_by_key(|(id, _)| *id);
        Ok(out)
    }

    /// Deletes subvolume `id` (never the system's): its tree loses its
    /// reference (what a snapshot still shares stays), its record goes.
    pub fn delete_subvol(&mut self, id: u64) -> Result<(), FsError> {
        if id == SYSTEM {
            return Err(FsError::ReadOnly);
        }
        let root = self.subvol(id)?.rec.root;
        self.release_tree(&root, id)?;
        self.subvols.remove(&id);
        let r = self.root;
        self.root = self.tree_delete(&r, tree::ROOT, &Key::new(id, kind::SUBVOL, 0))?.0;
        Ok(())
    }

    /// A home whose account was removed (TASK-104): renamed `retired:<id>`,
    /// read-only, its key forgotten. The key went with the account's record,
    /// so its sealed tree can't be walked to free its blocks: they stay used
    /// until FreallyFS tracks blocks by subvolume.
    pub fn retire_subvol(&mut self, id: u64) -> Result<(), FsError> {
        if id == SYSTEM {
            return Err(FsError::ReadOnly);
        }
        let s = self.subvol(id)?;
        s.rec.name = alloc::format!("retired:{id}");
        s.rec.flags |= crate::snapshot::READ_ONLY;
        s.changed = true;
        if self.keys.0.contains_key(&id) {
            self.lock(id)?;
        }
        Ok(())
    }

    /// Writes each changed subvolume's tree and then its record into the
    /// root tree (the commit's step 2).
    pub(crate) fn write_subvols(&mut self) -> Result<(), FsError> {
        let changed: alloc::vec::Vec<u64> = self.subvols.iter().filter(|(_, s)| s.changed).map(|(&id, _)| id).collect();
        for id in changed {
            let p = self.subvol(id)?.rec.root;
            let written = self.write_tree(p)?;
            let s = self.subvol(id)?;
            s.rec.root = written;
            s.changed = false;
            let bytes = s.rec.encode();
            let root = self.root;
            self.root = self.tree_insert(&root, tree::ROOT, Key::new(id, kind::SUBVOL, 0), &bytes)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_round_trips() {
        let r = SubvolRecord {
            root: BlockPtr { block: 99, generation: 4, check: [5; 16] },
            kind: 1,
            next_inode: 300,
            created_unix: 1_763_500_000,
            owner: [7; 16],
            key_check: [9; 48],
            name: String::from("home of Mike"),
            ..Default::default()
        };
        assert_eq!(SubvolRecord::decode(&r.encode()).unwrap(), r);
        assert!(SubvolRecord::decode(&[0; 10]).is_err());
    }
}
