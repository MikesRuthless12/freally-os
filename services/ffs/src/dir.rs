// SPDX-License-Identifier: Apache-2.0
//! Directories (§ 11): each entry under `(parent, DIR, FNV-1a 64 of the
//! name)`; an item holds one entry, or more when names' hashes collide.
//! Names are 1–255 bytes of UTF-8, never `/` or NUL, never `.` or `..`.

use crate::inode::{FileKind, Inode};
use crate::store::Store;
use crate::volume::Volume;
use crate::{FsError, Key, kind};
use alloc::string::String;
use alloc::vec::Vec;

/// FNV-1a, 64-bit.
pub fn name_hash(name: &str) -> u64 {
    name.bytes().fold(0xCBF2_9CE4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01B3))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub child: u64,
    pub kind: FileKind,
    pub name: String,
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 255 && name != "." && name != ".." && !name.bytes().any(|b| b == b'/' || b == 0)
}

fn encode(entries: &[DirEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in entries {
        out.extend_from_slice(&e.child.to_le_bytes());
        out.push(e.kind as u8);
        out.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
        out.extend_from_slice(e.name.as_bytes());
    }
    out
}

fn decode(mut b: &[u8]) -> Result<Vec<DirEntry>, FsError> {
    let mut out = Vec::new();
    while !b.is_empty() {
        if b.len() < 11 {
            return Err(FsError::Corrupt("a directory entry cut short"));
        }
        let child = u64::from_le_bytes(b[..8].try_into().unwrap_or_default());
        let kind = FileKind::from_u8(b[8])?;
        let n = u16::from_le_bytes([b[9], b[10]]) as usize;
        let name = b.get(11..11 + n).ok_or(FsError::Corrupt("a directory entry's name cut short"))?;
        let name = String::from(core::str::from_utf8(name).map_err(|_| FsError::Corrupt("a name that isn't UTF-8"))?);
        out.push(DirEntry { child, kind, name });
        b = &b[11 + n..];
    }
    Ok(out)
}

impl<S: Store> Volume<S> {
    fn dir_inode(&mut self, sv: u64, dir: u64) -> Result<Inode, FsError> {
        let i = self.stat(sv, dir)?;
        if i.kind != FileKind::Dir {
            return Err(FsError::NotADirectory);
        }
        Ok(i)
    }

    /// The entry called `name` in directory `dir`.
    pub fn lookup(&mut self, sv: u64, dir: u64, name: &str) -> Result<Option<DirEntry>, FsError> {
        let raw = self.sv_get(sv, &Key::new(dir, kind::DIR, name_hash(name)))?;
        Ok(match raw {
            Some(r) => decode(&r)?.into_iter().find(|e| e.name == name),
            None => None,
        })
    }

    /// Makes a file or directory called `name` in `dir`: its new inode.
    pub fn create(&mut self, sv: u64, dir: u64, name: &str, k: FileKind) -> Result<u64, FsError> {
        self.writable(sv)?;
        if !valid_name(name) {
            return Err(FsError::BadName);
        }
        let mut parent = self.dir_inode(sv, dir)?;
        let key = Key::new(dir, kind::DIR, name_hash(name));
        let mut list = match self.sv_get(sv, &key)? {
            Some(r) => decode(&r)?,
            None => Vec::new(),
        };
        if list.iter().any(|e| e.name == name) {
            return Err(FsError::Exists);
        }
        let ino = self.take_ino(sv)?;
        let now = self.now;
        let mut inode = Inode::new(k, dir, now);
        if k == FileKind::Dir {
            // Its own entry in its parent, and the parent's `..` count.
            inode.nlink = 2;
            parent.nlink += 1;
        }
        self.put_inode(sv, ino, &inode)?;
        list.push(DirEntry { child: ino, kind: k, name: String::from(name) });
        self.sv_insert(sv, key, &encode(&list))?;
        parent.size += 1;
        (parent.modified_ns, parent.changed_ns) = (now, now);
        self.put_inode(sv, dir, &parent)?;
        Ok(ino)
    }

    /// Every entry of directory `dir`, in hash order.
    pub fn read_dir(&mut self, sv: u64, dir: u64) -> Result<Vec<DirEntry>, FsError> {
        self.dir_inode(sv, dir)?;
        let root = self.sv_root(sv)?;
        let mut raw = Vec::new();
        self.tree_scan(&root, sv, &Key::new(dir, kind::DIR, 0), &mut |k, v| {
            if k.object != dir || k.kind != kind::DIR {
                return false;
            }
            raw.push(v.to_vec());
            true
        })?;
        let mut out = Vec::new();
        for r in raw {
            out.extend(decode(&r)?);
        }
        Ok(out)
    }

    /// Takes `name` out of `dir`. A directory must be empty; a file's last
    /// link takes its data and inode with it.
    pub fn unlink(&mut self, sv: u64, dir: u64, name: &str) -> Result<(), FsError> {
        self.writable(sv)?;
        let mut parent = self.dir_inode(sv, dir)?;
        let key = Key::new(dir, kind::DIR, name_hash(name));
        let mut list = match self.sv_get(sv, &key)? {
            Some(r) => decode(&r)?,
            None => return Err(FsError::NotFound),
        };
        let at = list.iter().position(|e| e.name == name).ok_or(FsError::NotFound)?;
        let e = list.remove(at);
        let mut child = self.stat(sv, e.child)?;
        if child.kind == FileKind::Dir {
            if child.size != 0 {
                return Err(FsError::NotEmpty);
            }
            parent.nlink -= 1;
            self.sv_delete(sv, &Key::new(e.child, kind::INODE, 0))?;
        } else {
            child.nlink -= 1;
            if child.nlink == 0 {
                self.truncate(sv, e.child, 0)?;
                self.sv_delete(sv, &Key::new(e.child, kind::INODE, 0))?;
            } else {
                child.changed_ns = self.now;
                self.put_inode(sv, e.child, &child)?;
            }
        }
        if list.is_empty() {
            self.sv_delete(sv, &key)?;
        } else {
            self.sv_insert(sv, key, &encode(&list))?;
        }
        parent.size -= 1;
        (parent.modified_ns, parent.changed_ns) = (self.now, self.now);
        self.put_inode(sv, dir, &parent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_and_entries() {
        // FNV-1a 64's published values.
        assert_eq!(name_hash(""), 0xCBF2_9CE4_8422_2325);
        assert_eq!(name_hash("a"), 0xAF63_DC4C_8601_EC8C);
        let list = alloc::vec![
            DirEntry { child: 300, kind: FileKind::File, name: String::from("notes.txt") },
            DirEntry { child: 301, kind: FileKind::Dir, name: String::from("Projects — 2026") },
        ];
        assert_eq!(decode(&encode(&list)).unwrap(), list);
        for bad in ["", ".", "..", "a/b", "nul\0"] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        assert!(valid_name(&"x".repeat(255)) && !valid_name(&"x".repeat(256)));
    }
}
