// SPDX-License-Identifier: Apache-2.0
//! The update index (`index.fui`, TASK-182, FR-061; PRD § 3): what an update
//! host offers one flavour, signed by a key the bootloader trusts. Its
//! entries name each package's version, the oldest version it updates, its
//! size and SHA-256, its file name and its notes, so Settings says what's new
//! and how big it is before a byte of a package moves.

use crate::{fup, key_id};
use alloc::string::String;
use alloc::vec::Vec;
use fcrypto::{ct, ed25519};

pub const MAGIC: [u8; 8] = *b"FRLYIDX1";
pub const FORMAT: u16 = 1;
const HEAD: usize = 24;
const ENTRY: usize = 160;
const TAIL: usize = 72;
pub const MAX_ENTRIES: usize = 8;
/// The largest an index can be: every entry with the longest notes.
pub const MAX_LEN: usize = HEAD + MAX_ENTRIES * (ENTRY + fup::MAX_NOTES) + TAIL;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexError {
    NotAnIndex,
    Format,
    /// The lengths don't add up, reserved bytes aren't zero, or a name or
    /// its notes aren't plain.
    Damaged,
    UnknownKey,
    BadSignature,
    WrongFlavour,
}

impl IndexError {
    pub fn describe(self) -> &'static str {
        match self {
            IndexError::NotAnIndex => "The update host didn't send a list of updates.",
            IndexError::Format => "The update list's format is newer than this system reads.",
            IndexError::Damaged => "The update list is damaged: its parts don't add up.",
            IndexError::UnknownKey => "The update list wasn't signed by a key this computer trusts.",
            IndexError::BadSignature => {
                "The update list's signature doesn't match it: it was changed after it was signed."
            }
            IndexError::WrongFlavour => "The update list is for another kind of Freally OS build.",
        }
    }
}

/// One package on offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub version: String,
    /// The oldest version it updates ("" for any).
    pub oldest: String,
    /// The whole `.fup`'s bytes, and their SHA-256.
    pub size: u64,
    pub sha256: [u8; 32],
    /// The file beside the index.
    pub name: String,
    pub notes: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Index {
    pub flavour: u8,
    pub issued: u64,
    pub entries: Vec<Entry>,
    /// Which of the trusted keys signed it.
    pub key_index: usize,
}

/// A NUL-padded field as text, or `None` if it isn't (bytes after the NUL,
/// or not UTF-8).
fn field(b: &[u8]) -> Option<String> {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    if b[end..].iter().any(|&c| c != 0) {
        return None;
    }
    core::str::from_utf8(&b[..end]).ok().map(String::from)
}

/// A package's file name: plain, and a `.fup`.
pub fn plain_name(n: &str) -> bool {
    n.len() > 4
        && n.ends_with(".fup")
        && !n.starts_with('.')
        && n.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_'))
}

/// The index checked: its signature by one of `trusted` first, then its
/// flavour (`flavour`, this system's) and every entry.
pub fn parse(b: &[u8], trusted: &[[u8; 32]], flavour: u8) -> Result<Index, IndexError> {
    if b.len() < HEAD + TAIL || b[..8] != MAGIC {
        return Err(IndexError::NotAnIndex);
    }
    if u16::from_le_bytes([b[8], b[9]]) != FORMAT {
        return Err(IndexError::Format);
    }
    if b.len() > MAX_LEN {
        return Err(IndexError::Damaged);
    }
    let signed = b.len() - 64;
    let mut kid = [0u8; 8];
    kid.copy_from_slice(&b[signed - 8..signed]);
    let key_index = trusted.iter().position(|k| ct::eq(&key_id(k), &kid)).ok_or(IndexError::UnknownKey)?;
    let mut sig = [0u8; 64];
    sig.copy_from_slice(&b[signed..]);
    if !ed25519::verify(&trusted[key_index], &b[..signed], &sig) {
        return Err(IndexError::BadSignature);
    }
    let count = b[11] as usize;
    if count == 0 || count > MAX_ENTRIES || b[12..16] != [0; 4] {
        return Err(IndexError::Damaged);
    }
    if b[10] != flavour {
        return Err(IndexError::WrongFlavour);
    }
    let mut issued = [0u8; 8];
    issued.copy_from_slice(&b[16..24]);
    let end = b.len() - TAIL;
    let mut at = HEAD;
    let mut entries = Vec::new();
    for _ in 0..count {
        let h = b.get(at..at + ENTRY).ok_or(IndexError::Damaged)?;
        let notes_len = u32::from_le_bytes([h[136], h[137], h[138], h[139]]) as usize;
        if notes_len > fup::MAX_NOTES || h[140..ENTRY].iter().any(|&c| c != 0) {
            return Err(IndexError::Damaged);
        }
        let notes = b.get(at + ENTRY..at + ENTRY + notes_len).ok_or(IndexError::Damaged)?;
        if at + ENTRY + notes_len > end {
            return Err(IndexError::Damaged);
        }
        let mut size = [0u8; 8];
        size.copy_from_slice(&h[64..72]);
        let mut sha256 = [0u8; 32];
        sha256.copy_from_slice(&h[72..104]);
        let e = Entry {
            version: field(&h[..32]).filter(|v| !v.is_empty()).ok_or(IndexError::Damaged)?,
            oldest: field(&h[32..64]).ok_or(IndexError::Damaged)?,
            size: u64::from_le_bytes(size),
            sha256,
            name: field(&h[104..136]).filter(|n| plain_name(n)).ok_or(IndexError::Damaged)?,
            notes: String::from(core::str::from_utf8(notes).map_err(|_| IndexError::Damaged)?),
        };
        if e.size <= fup::HEADER as u64 {
            return Err(IndexError::Damaged);
        }
        entries.push(e);
        at += ENTRY + notes_len;
    }
    if at != end {
        return Err(IndexError::Damaged);
    }
    Ok(Index { flavour, issued: u64::from_le_bytes(issued), entries, key_index })
}

/// What to offer a system running `running`: the newest entry newer than
/// it whose oldest version is at most it.
pub fn offer<'a>(ix: &'a Index, running: &str) -> Option<&'a Entry> {
    let now = fup::numbers(running);
    ix.entries
        .iter()
        .filter(|e| fup::numbers(&e.version) > now)
        .filter(|e| e.oldest.is_empty() || fup::numbers(&e.oldest) <= now)
        .max_by_key(|e| fup::numbers(&e.version))
}

/// An index for `flavour`, signed with `seed`.
#[cfg(feature = "build")]
pub fn make(flavour: u8, issued: u64, entries: &[Entry], seed: &[u8; 32]) -> Result<Vec<u8>, &'static str> {
    if entries.is_empty() || entries.len() > MAX_ENTRIES || flavour > 2 {
        return Err("an index holds 1 to 8 entries, for a known flavour");
    }
    let mut b = alloc::vec![0u8; HEAD];
    b[..8].copy_from_slice(&MAGIC);
    b[8..10].copy_from_slice(&FORMAT.to_le_bytes());
    b[10] = flavour;
    b[11] = entries.len() as u8;
    b[16..24].copy_from_slice(&issued.to_le_bytes());
    for e in entries {
        if e.version.is_empty() || e.version.len() > 32 || e.oldest.len() > 32 {
            return Err("a version is empty or longer than 32 bytes");
        }
        if !plain_name(&e.name) || e.name.len() > 32 {
            return Err("a package's name isn't plain");
        }
        if e.notes.len() > fup::MAX_NOTES {
            return Err("the release notes are longer than 16 KiB");
        }
        let mut h = [0u8; ENTRY];
        h[..e.version.len()].copy_from_slice(e.version.as_bytes());
        h[32..32 + e.oldest.len()].copy_from_slice(e.oldest.as_bytes());
        h[64..72].copy_from_slice(&e.size.to_le_bytes());
        h[72..104].copy_from_slice(&e.sha256);
        h[104..104 + e.name.len()].copy_from_slice(e.name.as_bytes());
        h[136..140].copy_from_slice(&(e.notes.len() as u32).to_le_bytes());
        b.extend_from_slice(&h);
        b.extend_from_slice(e.notes.as_bytes());
    }
    b.extend_from_slice(&key_id(&ed25519::public_key(seed)));
    let sig = ed25519::sign(seed, &b);
    b.extend_from_slice(&sig);
    Ok(b)
}

#[cfg(all(test, feature = "build"))]
mod tests {
    use super::*;

    const SEED: [u8; 32] = [7; 32];

    fn entry(v: &str, oldest: &str) -> Entry {
        Entry {
            version: v.into(),
            oldest: oldest.into(),
            size: 200_000_000,
            sha256: [0x5A; 32],
            name: alloc::format!("freally-{v}.fup"),
            notes: alloc::format!("# {v}\n\nWhat's new."),
        }
    }

    fn trusted() -> [[u8; 32]; 1] {
        [ed25519::public_key(&SEED)]
    }

    #[test]
    fn an_index_round_trips() {
        let es = [entry("0.9.1", "0.9.0"), entry("0.9.2", "")];
        let b = make(2, 1_795_543_500, &es, &SEED).unwrap();
        let ix = parse(&b, &trusted(), 2).unwrap();
        assert_eq!((ix.flavour, ix.issued, ix.entries.as_slice()), (2, 1_795_543_500, &es[..]));
    }

    #[test]
    fn every_changed_byte_is_refused() {
        let b = make(2, 1, &[entry("0.9.1", "")], &SEED).unwrap();
        for i in 0..b.len() {
            let mut c = b.clone();
            c[i] ^= 0x01;
            assert!(parse(&c, &trusted(), 2).is_err(), "byte {i}");
        }
        assert_eq!(parse(&b[..b.len() - 1], &trusted(), 2), Err(IndexError::UnknownKey));
        let mut longer = b.clone();
        longer.push(0);
        assert!(parse(&longer, &trusted(), 2).is_err());
    }

    #[test]
    fn a_foreign_key_another_flavour_and_odd_parts_are_refused() {
        let b = make(2, 1, &[entry("0.9.1", "")], &[9; 32]).unwrap();
        assert_eq!(parse(&b, &trusted(), 2), Err(IndexError::UnknownKey));
        let b = make(1, 1, &[entry("0.9.1", "")], &SEED).unwrap();
        assert_eq!(parse(&b, &trusted(), 2), Err(IndexError::WrongFlavour));
        assert_eq!(
            parse(b"FRLYFUP1 not an index at all, but long enough to read its header", &trusted(), 2),
            Err(IndexError::NotAnIndex)
        );
        for name in ["../up.fup", "up.bin", ".fup", "a b.fup", "up/x.fup"] {
            let mut e = entry("0.9.1", "");
            e.name = name.into();
            assert!(make(2, 1, &[e], &SEED).is_err(), "{name}");
        }
        assert!(make(2, 1, &[], &SEED).is_err());
    }

    #[test]
    fn the_offer_is_the_newest_that_may_go_on() {
        let b = make(2, 1, &[entry("0.9.1", "0.9.0"), entry("0.9.3", "0.9.2"), entry("0.9.2", "")], &SEED).unwrap();
        let ix = parse(&b, &trusted(), 2).unwrap();
        assert_eq!(offer(&ix, "0.9.0").map(|e| e.version.as_str()), Some("0.9.2"), "0.9.3 needs 0.9.2 first");
        assert_eq!(offer(&ix, "0.9.2").map(|e| e.version.as_str()), Some("0.9.3"));
        assert_eq!(offer(&ix, "0.9.3"), None, "up to date");
        assert_eq!(offer(&ix, "0.10.0"), None, "nothing older is offered");
    }
}
