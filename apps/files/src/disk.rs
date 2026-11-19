// SPDX-License-Identifier: Apache-2.0
//! What Files does to the disk, behind a trait: `Vfs` is the real one (the
//! file protocol, through `fstd::fs`); the tests use `Mem`, a tree in RAM.

use alloc::string::String;
use alloc::vec::Vec;

/// What's at a path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    pub dir: bool,
    pub size: u64,
    pub modified_ns: u64,
}

/// A refusal, in words for the person (vfs's own, or ours).
pub type Said = String;

pub trait Disk {
    /// A directory's entries: each name and whether it's a directory.
    fn list(&mut self, path: &str) -> Result<Vec<(String, bool)>, Said>;
    fn meta(&mut self, path: &str) -> Result<Meta, Said>;
    fn mkdir(&mut self, path: &str) -> Result<(), Said>;
    fn rename(&mut self, from: &str, to: &str) -> Result<(), Said>;
    /// A file, or an empty directory.
    fn remove(&mut self, path: &str) -> Result<(), Said>;
    fn read(&mut self, path: &str) -> Result<Vec<u8>, Said>;
    fn write(&mut self, path: &str, bytes: &[u8]) -> Result<(), Said>;

    fn exists(&mut self, path: &str) -> bool {
        self.meta(path).is_ok()
    }

    /// A file, or a directory and everything in it.
    fn remove_all(&mut self, path: &str) -> Result<(), Said> {
        if self.meta(path)?.dir {
            for (name, _) in self.list(path)? {
                self.remove_all(&join(path, &name))?;
            }
        }
        self.remove(path)
    }
}

/// `dir` and `name`, with one slash between.
pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') { alloc::format!("{dir}{name}") } else { alloc::format!("{dir}/{name}") }
}

/// The directory `path` is in, and its name.
pub fn split(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(0) => ("/", &path[1..]),
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    }
}

/// A name a person may give a file or folder: not empty, no slash, not
/// `.` or `..`, at most 255 bytes, no control characters, and not one of
/// Files' own hidden names (a leading dot).
pub fn valid_name(name: &str) -> Result<(), &'static str> {
    if name.trim().is_empty() {
        return Err("A name can't be empty.");
    }
    if name.contains('/') {
        return Err("A name can't have a / in it.");
    }
    if name.starts_with('.') {
        return Err("A name can't start with a dot.");
    }
    if name.len() > 255 {
        return Err("That name is too long.");
    }
    if name.chars().any(char::is_control) {
        return Err("A name can't have control characters in it.");
    }
    Ok(())
}

/// The file protocol, through `fstd::fs`.
pub struct Vfs;

fn said(e: fstd::fs::FsError) -> Said {
    e.text
}

impl Disk for Vfs {
    fn list(&mut self, path: &str) -> Result<Vec<(String, bool)>, Said> {
        fstd::fs::read_dir(path).map_err(said)
    }
    fn meta(&mut self, path: &str) -> Result<Meta, Said> {
        fstd::fs::metadata(path).map(|s| Meta { dir: s.dir, size: s.size, modified_ns: s.modified_ns }).map_err(said)
    }
    fn mkdir(&mut self, path: &str) -> Result<(), Said> {
        fstd::fs::create_dir(path).map_err(said)
    }
    fn rename(&mut self, from: &str, to: &str) -> Result<(), Said> {
        fstd::fs::rename(from, to).map_err(said)
    }
    fn remove(&mut self, path: &str) -> Result<(), Said> {
        fstd::fs::remove(path).map_err(said)
    }
    fn read(&mut self, path: &str) -> Result<Vec<u8>, Said> {
        fstd::fs::read(path).map_err(said)
    }
    fn write(&mut self, path: &str, bytes: &[u8]) -> Result<(), Said> {
        fstd::fs::write(path, bytes).map_err(said)
    }
}

/// A tree in RAM, for the tests.
#[cfg(test)]
#[derive(Default)]
pub struct Mem {
    /// Each path, and its bytes (`None`: a directory).
    pub items: alloc::collections::BTreeMap<String, Option<Vec<u8>>>,
    pub now: u64,
}

#[cfg(test)]
impl Mem {
    pub fn with(paths: &[&str]) -> Mem {
        let mut m = Mem::default();
        m.items.insert(String::from("/"), None);
        for p in paths {
            let dir = p.ends_with('/');
            let p = p.trim_end_matches('/');
            m.items.insert(String::from(p), if dir { None } else { Some(Vec::from(p.as_bytes())) });
        }
        m
    }

    fn children(&self, path: &str) -> Vec<String> {
        let prefix = if path == "/" { String::from("/") } else { alloc::format!("{path}/") };
        self.items
            .keys()
            .filter(|k| k.starts_with(&prefix) && k.len() > prefix.len() && !k[prefix.len()..].contains('/'))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
impl Disk for Mem {
    fn list(&mut self, path: &str) -> Result<Vec<(String, bool)>, Said> {
        match self.items.get(path) {
            Some(None) => Ok(self
                .children(path)
                .into_iter()
                .map(|k| {
                    let dir = self.items[&k].is_none();
                    (String::from(split(&k).1), dir)
                })
                .collect()),
            _ => Err(String::from("not a folder")),
        }
    }
    fn meta(&mut self, path: &str) -> Result<Meta, Said> {
        match self.items.get(path) {
            Some(None) => Ok(Meta { dir: true, size: 0, modified_ns: self.now }),
            Some(Some(b)) => Ok(Meta { dir: false, size: b.len() as u64, modified_ns: self.now }),
            None => Err(String::from("not found")),
        }
    }
    fn mkdir(&mut self, path: &str) -> Result<(), Said> {
        if self.items.contains_key(path) {
            return Err(String::from("already exists"));
        }
        self.items.insert(String::from(path), None);
        Ok(())
    }
    fn rename(&mut self, from: &str, to: &str) -> Result<(), Said> {
        if self.items.contains_key(to) {
            return Err(String::from("already exists"));
        }
        if !self.items.contains_key(split(to).0) {
            return Err(String::from("no such folder"));
        }
        let moved: Vec<String> =
            self.items.keys().filter(|k| *k == from || k.starts_with(&alloc::format!("{from}/"))).cloned().collect();
        if moved.is_empty() {
            return Err(String::from("not found"));
        }
        for k in moved {
            let v = self.items.remove(&k).unwrap_or(None);
            self.items.insert(alloc::format!("{to}{}", &k[from.len()..]), v);
        }
        Ok(())
    }
    fn remove(&mut self, path: &str) -> Result<(), Said> {
        if !self.children(path).is_empty() {
            return Err(String::from("not empty"));
        }
        self.items.remove(path).map(|_| ()).ok_or(String::from("not found"))
    }
    fn read(&mut self, path: &str) -> Result<Vec<u8>, Said> {
        self.items.get(path).cloned().flatten().ok_or(String::from("not a file"))
    }
    fn write(&mut self, path: &str, bytes: &[u8]) -> Result<(), Said> {
        self.items.insert(String::from(path), Some(Vec::from(bytes)));
        Ok(())
    }
}
