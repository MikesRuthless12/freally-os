// SPDX-License-Identifier: Apache-2.0
//! Bundles (TASK-059): many small files as one, so a service gets a whole
//! set (the hardware set's FIMGs at one scale, both themes) in one VMO
//! instead of a handle each.
//!
//! The format, little-endian: `FBND`, version (u16, 1), the number of
//! entries (u16); then each entry's name length (u16), its name (UTF-8),
//! its offset from the bundle's start and its length (u32 each); then the
//! files. Names are unique and sorted.

use alloc::vec::Vec;

pub const MAGIC: [u8; 4] = *b"FBND";
pub const VERSION: u16 = 1;

/// A bundle of `files` (name, bytes); names sorted, the first of any repeat kept.
pub fn build(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut files: Vec<&(&str, &[u8])> = files.iter().collect();
    files.sort_by_key(|f| f.0);
    files.dedup_by_key(|f| f.0);
    let index_len: usize = files.iter().map(|f| 2 + f.0.len() + 8).sum();
    let mut at = (8 + index_len) as u32;
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    for (name, bytes) in &files {
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&at.to_le_bytes());
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        at += bytes.len() as u32;
    }
    for (_, bytes) in &files {
        out.extend_from_slice(bytes);
    }
    out
}

/// A bundle read in place.
#[derive(Clone, Copy, Debug)]
pub struct Bundle<'a> {
    bytes: &'a [u8],
    count: u16,
}

impl<'a> Bundle<'a> {
    /// Checks the header and that every entry lies inside the bundle.
    pub fn parse(bytes: &'a [u8]) -> Result<Bundle<'a>, &'static str> {
        if bytes.get(..4) != Some(&MAGIC[..]) {
            return Err("not a bundle");
        }
        let u16_at = |i: usize| bytes.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        if u16_at(4) != Some(VERSION) {
            return Err("a bundle of another version");
        }
        let count = u16_at(6).ok_or("a bundle shorter than its header")?;
        let b = Bundle { bytes, count };
        for e in b.entries() {
            e?;
        }
        Ok(b)
    }

    /// Every entry: (name, offset, length), or why it's damaged.
    fn entries(&self) -> impl Iterator<Item = Result<(&'a str, usize, usize), &'static str>> + '_ {
        let bytes = self.bytes;
        let mut at = 8usize;
        (0..self.count).map(move |_| {
            let n = bytes.get(at..at + 2).ok_or("a bundle's index runs past its end")?;
            let n = u16::from_le_bytes([n[0], n[1]]) as usize;
            let name = bytes.get(at + 2..at + 2 + n).ok_or("a bundle's index runs past its end")?;
            let name = core::str::from_utf8(name).map_err(|_| "a name that isn't UTF-8")?;
            let w = bytes.get(at + 2 + n..at + 10 + n).ok_or("a bundle's index runs past its end")?;
            let (off, len) = (
                u32::from_le_bytes([w[0], w[1], w[2], w[3]]) as usize,
                u32::from_le_bytes([w[4], w[5], w[6], w[7]]) as usize,
            );
            if off.checked_add(len).is_none_or(|end| end > bytes.len()) {
                return Err("a file runs past the bundle's end");
            }
            at += 10 + n;
            Ok((name, off, len))
        })
    }

    /// The file called `name`.
    pub fn get(&self, name: &str) -> Option<&'a [u8]> {
        let bytes = self.bytes;
        self.entries().flatten().find(|e| e.0 == name).map(|(_, off, len)| &bytes[off..off + len])
    }

    /// The name of the file whose bytes hold `addr` (a piece the crisp guard
    /// caught, found by where its pixels are).
    pub fn name_of(&self, addr: usize) -> Option<&'a str> {
        let base = self.bytes.as_ptr() as usize;
        let at = addr.checked_sub(base)?;
        self.entries().flatten().find(|&(_, off, len)| at >= off && at < off + len).map(|e| e.0)
    }

    pub fn len(&self) -> usize {
        self.count as usize
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_go_in_and_come_out() {
        let b = build(&[("dark/b", b"two"), ("dark/a", b"one!"), ("light/a", b"")]);
        let r = Bundle::parse(&b).unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!(r.get("dark/a"), Some(&b"one!"[..]));
        assert_eq!(r.get("dark/b"), Some(&b"two"[..]));
        assert_eq!(r.get("light/a"), Some(&b""[..]));
        assert_eq!(r.get("nope"), None);
        assert_eq!(build(&[("x", b"1"), ("x", b"2")]).len(), build(&[("x", b"1")]).len(), "a repeat is dropped");
    }

    #[test]
    fn damage_is_refused() {
        let good = build(&[("a", b"hello")]);
        assert!(Bundle::parse(b"FBNX\x01\x00\x00\x00").is_err());
        let mut v2 = good.clone();
        v2[4] = 2;
        assert!(Bundle::parse(&v2).is_err());
        let short = &good[..good.len() - 1];
        assert_eq!(Bundle::parse(short).unwrap_err(), "a file runs past the bundle's end");
        let mut lie = good.clone();
        lie[6] = 9; // nine entries claimed
        assert!(Bundle::parse(&lie).is_err());
    }
}
