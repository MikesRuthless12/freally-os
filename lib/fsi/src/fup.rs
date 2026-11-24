// SPDX-License-Identifier: Apache-2.0
//! The update package (`.fup`, TASK-110, FR-050; PRD § 3): a signed
//! 256-byte header, the release notes, then the `.fsi` image. The header is
//! checked first (its signature by a key the bootloader trusts, the
//! flavour, the lengths), then the notes and the image against its hashes,
//! then the image itself (`verify`), which must name the header's version.

use crate::{FsiError, key_id, verify};
use fcrypto::{ct, ed25519, sha2::sha256};

pub const MAGIC: [u8; 8] = *b"FRLYFUP1";
pub const FORMAT: u16 = 1;
pub const HEADER: usize = 256;
/// Bytes 0–159 are signed.
const SIGNED: usize = 160;
pub const MAX_NOTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FupError {
    /// Not a `.fup` at all.
    NotAnUpdate,
    /// A newer format than this system reads.
    Format,
    /// The lengths don't add up, or reserved bytes aren't zero.
    Damaged,
    UnknownKey,
    BadSignature,
    /// For a dev, e2e or release system other than this one.
    WrongFlavour,
    /// The notes or the image don't match what was signed.
    Changed,
    Image(FsiError),
    /// The version this system already runs.
    SameVersion,
    /// This system is older than the oldest the update updates.
    TooOld,
}

impl FupError {
    pub fn describe(self) -> &'static str {
        match self {
            FupError::NotAnUpdate => "This file isn't a Freally OS update.",
            FupError::Format => "This update's format is newer than this system reads.",
            FupError::Damaged => "This update is damaged: its parts don't add up.",
            FupError::UnknownKey => "This update wasn't signed by a key this computer trusts.",
            FupError::BadSignature => "This update's signature doesn't match it: it was changed after it was signed.",
            FupError::WrongFlavour => "This update is for another kind of Freally OS build.",
            FupError::Changed => "This update was changed after it was signed.",
            FupError::Image(e) => e.describe(),
            FupError::SameVersion => "This computer already runs that version.",
            FupError::TooOld => {
                "This version of Freally OS is too old for that update: install the ones before it first."
            }
        }
    }
}

/// A header that checked out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    pub flavour: u8,
    pub notes_len: u32,
    pub image_len: u64,
    pub version: [u8; 32],
    pub oldest: [u8; 32],
    pub notes_hash: [u8; 32],
    pub image_hash: [u8; 32],
    /// Which of the trusted keys signed it.
    pub key_index: usize,
}

fn text(b: &[u8; 32]) -> &str {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    core::str::from_utf8(&b[..end]).unwrap_or("")
}

impl Head {
    /// The version it brings.
    pub fn version(&self) -> &str {
        text(&self.version)
    }

    /// The oldest version it updates ("" for any).
    pub fn oldest(&self) -> &str {
        text(&self.oldest)
    }

    /// The whole file's length.
    pub fn total(&self) -> u64 {
        HEADER as u64 + self.notes_len as u64 + self.image_len
    }
}

/// The header checked: its format and lengths, the signature by one of
/// `trusted`, and the flavour (`flavour`, this system's).
pub fn head(b: &[u8], trusted: &[[u8; 32]], flavour: u8) -> Result<Head, FupError> {
    if b.len() < HEADER || b[..8] != MAGIC {
        return Err(FupError::NotAnUpdate);
    }
    if u16::from_le_bytes([b[8], b[9]]) != FORMAT {
        return Err(FupError::Format);
    }
    let arr = |o: usize| -> [u8; 32] {
        let mut a = [0u8; 32];
        a.copy_from_slice(&b[o..o + 32]);
        a
    };
    let mut kid = [0u8; 8];
    kid.copy_from_slice(&b[152..160]);
    let key_index = trusted.iter().position(|k| ct::eq(&key_id(k), &kid)).ok_or(FupError::UnknownKey)?;
    let mut sig = [0u8; 64];
    sig.copy_from_slice(&b[160..224]);
    if !ed25519::verify(&trusted[key_index], &b[..SIGNED], &sig) {
        return Err(FupError::BadSignature);
    }
    let notes_len = u32::from_le_bytes([b[12], b[13], b[14], b[15]]);
    let mut il = [0u8; 8];
    il.copy_from_slice(&b[16..24]);
    let image_len = u64::from_le_bytes(il);
    if b[11] != 0 || b[224..HEADER].iter().any(|&c| c != 0) || notes_len as usize > MAX_NOTES || image_len == 0 {
        return Err(FupError::Damaged);
    }
    if b[10] != flavour {
        return Err(FupError::WrongFlavour);
    }
    Ok(Head {
        flavour: b[10],
        notes_len,
        image_len,
        version: arr(24),
        oldest: arr(56),
        notes_hash: arr(88),
        image_hash: arr(120),
        key_index,
    })
}

/// The notes, checked against the header.
pub fn notes<'a>(h: &Head, notes: &'a [u8]) -> Result<&'a str, FupError> {
    if notes.len() != h.notes_len as usize {
        return Err(FupError::Damaged);
    }
    if !ct::eq(&sha256(notes), &h.notes_hash) {
        return Err(FupError::Changed);
    }
    core::str::from_utf8(notes).map_err(|_| FupError::Damaged)
}

/// "0.8.0" (and "0.8.0-test") as numbers to compare.
pub(crate) fn numbers(v: &str) -> [u64; 3] {
    let mut out = [0u64; 3];
    let core = v.split(['-', '+']).next().unwrap_or("");
    for (i, p) in core.split('.').take(3).enumerate() {
        out[i] = p.parse().unwrap_or(0);
    }
    out
}

/// Whether it may go onto a system running `running`: not the same
/// version, and `running` at least the oldest it updates.
pub fn may_install(h: &Head, running: &str) -> Result<(), FupError> {
    if h.version() == running {
        return Err(FupError::SameVersion);
    }
    if !h.oldest().is_empty() && numbers(running) < numbers(h.oldest()) {
        return Err(FupError::TooOld);
    }
    Ok(())
}

/// The image, whole: its hash, then `verify` with `trusted`, and that it
/// names the header's version.
pub fn check_image(h: &Head, image: &[u8], trusted: &[[u8; 32]]) -> Result<(), FupError> {
    if image.len() as u64 != h.image_len {
        return Err(FupError::Damaged);
    }
    if !ct::eq(&sha256(image), &h.image_hash) {
        return Err(FupError::Changed);
    }
    let v = verify(image, trusted).map_err(FupError::Image)?;
    if v.header.version != h.version || v.header.flavour != h.flavour as u16 {
        return Err(FupError::Changed);
    }
    Ok(())
}

/// A `.fup` made from a signed image (the version and flavour are the
/// image's), signed with `seed` (the image's own key).
#[cfg(feature = "build")]
pub fn make(oldest: &str, notes: &str, image: &[u8], seed: &[u8; 32]) -> Result<alloc::vec::Vec<u8>, &'static str> {
    let h = crate::Header::parse(image).map_err(|e| e.describe())?;
    if notes.len() > MAX_NOTES {
        return Err("the release notes are longer than 16 KiB");
    }
    if oldest.len() > 32 || h.flavour > 2 {
        return Err("the oldest version is longer than 32 bytes, or the flavour is unknown");
    }
    let public = ed25519::public_key(seed);
    if key_id(&public) != h.key_id {
        return Err("the image wasn't made for that key");
    }
    let mut b = alloc::vec![0u8; HEADER];
    b[..8].copy_from_slice(&MAGIC);
    b[8..10].copy_from_slice(&FORMAT.to_le_bytes());
    b[10] = h.flavour as u8;
    b[12..16].copy_from_slice(&(notes.len() as u32).to_le_bytes());
    b[16..24].copy_from_slice(&(image.len() as u64).to_le_bytes());
    b[24..56].copy_from_slice(&h.version);
    b[56..56 + oldest.len()].copy_from_slice(oldest.as_bytes());
    b[88..120].copy_from_slice(&sha256(notes.as_bytes()));
    b[120..152].copy_from_slice(&sha256(image));
    b[152..160].copy_from_slice(&h.key_id);
    let sig = ed25519::sign(seed, &b[..SIGNED]);
    b[160..224].copy_from_slice(&sig);
    b.extend_from_slice(notes.as_bytes());
    b.extend_from_slice(image);
    Ok(b)
}

/// The image's first block (its signed header), checked on its own: the
/// updater writes an image as it comes and holds this block back until the
/// whole image matches the hash the `.fup` header signed. Its signature by
/// one of `trusted`, and the version, flavour and length the header says.
pub fn check_image_head(h: &Head, first: &[u8], trusted: &[[u8; 32]]) -> Result<(), FupError> {
    let ih = crate::Header::parse(first).map_err(FupError::Image)?;
    let k = trusted.iter().position(|k| ct::eq(&key_id(k), &ih.key_id)).ok_or(FupError::Image(FsiError::UnknownKey))?;
    if !ed25519::verify(&trusted[k], &first[..crate::SIGNED_LEN], &ih.signature) {
        return Err(FupError::Image(FsiError::BadSignature));
    }
    if ih.version != h.version || ih.flavour != h.flavour as u16 || ih.image_len != h.image_len {
        return Err(FupError::Changed);
    }
    Ok(())
}

/// A package taken apart, unchecked (FR-051: the release pipeline makes it
/// again around its image signed): the oldest version it updates, its notes
/// and its image.
#[cfg(feature = "build")]
pub fn unpack(b: &[u8]) -> Result<(&str, &str, &[u8]), &'static str> {
    if b.len() < HEADER || b[..8] != MAGIC {
        return Err("not an update package");
    }
    let notes_len = u32::from_le_bytes([b[12], b[13], b[14], b[15]]) as usize;
    let mut il = [0u8; 8];
    il.copy_from_slice(&b[16..24]);
    let image_len = usize::try_from(u64::from_le_bytes(il)).map_err(|_| "its lengths don't add up")?;
    if HEADER.checked_add(notes_len).and_then(|n| n.checked_add(image_len)) != Some(b.len()) {
        return Err("its lengths don't add up");
    }
    let end = b[56..88].iter().position(|&c| c == 0).unwrap_or(32);
    let oldest = core::str::from_utf8(&b[56..56 + end]).map_err(|_| "its oldest version isn't text")?;
    let notes = core::str::from_utf8(&b[HEADER..HEADER + notes_len]).map_err(|_| "its notes aren't text")?;
    Ok((oldest, notes, &b[HEADER + notes_len..]))
}
