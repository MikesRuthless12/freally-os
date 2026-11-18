// SPDX-License-Identifier: Apache-2.0
//! The table of contents: 160-byte entries, sorted by path.

use crate::{BLOCK, FsiError, TOC_ENTRY_LEN};

/// What a file in the image is.
#[repr(u16)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Kernel = 1,
    Conductor = 2,
    Service = 3,
    Driver = 4,
    App = 5,
    Manifest = 6,
    Asset = 7,
    Font = 8,
    Data = 9,
}

impl Kind {
    pub fn from_u16(v: u16) -> Option<Kind> {
        Some(match v {
            1 => Kind::Kernel,
            2 => Kind::Conductor,
            3 => Kind::Service,
            4 => Kind::Driver,
            5 => Kind::App,
            6 => Kind::Manifest,
            7 => Kind::Asset,
            8 => Kind::Font,
            9 => Kind::Data,
            _ => return None,
        })
    }
}

pub const FLAG_DEV_ONLY: u16 = 1 << 0;
pub const FLAG_E2E_ONLY: u16 = 1 << 1;

/// One TOC entry, as read.
#[derive(Clone, Copy, Debug)]
pub struct TocEntry {
    pub path: [u8; 96],
    pub kind: u16,
    pub flags: u16,
    pub offset: u64,
    pub len: u64,
    pub sha256: [u8; 32],
}

impl TocEntry {
    pub fn path(&self) -> &str {
        let end = self.path.iter().position(|&b| b == 0).unwrap_or(self.path.len());
        core::str::from_utf8(&self.path[..end]).unwrap_or("")
    }

    pub(crate) fn read(b: &[u8]) -> TocEntry {
        let mut path = [0u8; 96];
        path.copy_from_slice(&b[0..96]);
        let mut sha = [0u8; 32];
        sha.copy_from_slice(&b[120..152]);
        let u64_at = |o: usize| {
            let mut a = [0u8; 8];
            a.copy_from_slice(&b[o..o + 8]);
            u64::from_le_bytes(a)
        };
        TocEntry {
            path,
            kind: u16::from_le_bytes([b[96], b[97]]),
            flags: u16::from_le_bytes([b[98], b[99]]),
            offset: u64_at(104),
            len: u64_at(112),
            sha256: sha,
        }
    }

    pub fn write(&self, out: &mut [u8]) {
        out[..TOC_ENTRY_LEN].fill(0);
        out[0..96].copy_from_slice(&self.path);
        out[96..98].copy_from_slice(&self.kind.to_le_bytes());
        out[98..100].copy_from_slice(&self.flags.to_le_bytes());
        out[104..112].copy_from_slice(&self.offset.to_le_bytes());
        out[112..120].copy_from_slice(&self.len.to_le_bytes());
        out[120..152].copy_from_slice(&self.sha256);
    }
}

/// Walks the TOC's entries in order.
#[derive(Clone, Copy, Debug)]
pub struct TocIter<'a> {
    rest: &'a [u8],
}

impl<'a> TocIter<'a> {
    pub(crate) fn new(toc: &'a [u8]) -> TocIter<'a> {
        TocIter { rest: toc }
    }
}

impl Iterator for TocIter<'_> {
    type Item = TocEntry;

    fn next(&mut self) -> Option<TocEntry> {
        if self.rest.len() < TOC_ENTRY_LEN {
            return None;
        }
        let (e, rest) = self.rest.split_at(TOC_ENTRY_LEN);
        self.rest = rest;
        Some(TocEntry::read(e))
    }
}

/// Every entry well formed, sorted, unique, block-aligned and inside the data region.
pub(crate) fn check(toc: &[u8], image_len: u64, data_end: u64) -> Result<(), FsiError> {
    let toc_end = BLOCK as u64 + toc.len() as u64;
    let mut prev: Option<[u8; 96]> = None;
    for raw in toc.chunks(TOC_ENTRY_LEN) {
        let e = TocEntry::read(raw);
        let path_len = e.path.iter().position(|&b| b == 0).unwrap_or(96);
        let well_formed = path_len > 1
            && e.path[0] == b'/'
            && e.path[path_len..].iter().all(|&b| b == 0)
            && core::str::from_utf8(&e.path[..path_len]).is_ok()
            && Kind::from_u16(e.kind).is_some()
            && e.flags & !(FLAG_DEV_ONLY | FLAG_E2E_ONLY) == 0
            && raw[100..104].iter().all(|&b| b == 0)
            && raw[152..160].iter().all(|&b| b == 0)
            && e.offset % BLOCK as u64 == 0
            && e.offset >= toc_end
            && e.offset.checked_add(e.len).is_some_and(|end| end <= data_end && end <= image_len);
        if !well_formed {
            return Err(FsiError::BadToc);
        }
        if let Some(p) = prev
            && p >= e.path
        {
            return Err(FsiError::BadToc);
        }
        prev = Some(e.path);
    }
    Ok(())
}
