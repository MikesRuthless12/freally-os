// SPDX-License-Identifier: Apache-2.0
//! The Freally system image (`.fsi`), format 1. The byte layout is in
//! `docs/fsi-format.md`; this crate reads and verifies it with no allocation
//! (for the bootloader), and builds and signs it with the `build` feature.
#![no_std]

#[cfg(feature = "build")]
extern crate alloc;

#[cfg(feature = "build")]
mod build;
mod merkle;
mod toc;

#[cfg(feature = "build")]
pub use build::{ImageBuilder, sign};
pub use merkle::tree_nodes;
pub use toc::{Kind, TocEntry, TocIter};

use fcrypto::{ct, ed25519, sha2::sha256};

pub const BLOCK: usize = 4096;
pub const MAGIC: [u8; 8] = *b"FRLYSI01";
pub const FORMAT: u16 = 1;
pub const HEADER_LEN: usize = BLOCK;
/// The signature covers bytes `[0, SIGNED_LEN)` of the header.
pub const SIGNED_LEN: usize = 4032;
pub const TOC_ENTRY_LEN: usize = 160;

/// Why an image was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FsiError {
    /// Shorter than a header.
    TooShort,
    BadMagic,
    UnknownFormat(u16),
    UnknownFlavour(u16),
    /// No key this bootloader embeds has this key id.
    UnknownKey,
    BadSignature,
    /// A reserved byte isn't zero.
    ReservedNotZero,
    /// The lengths and offsets in the header don't add up.
    BadLayout,
    /// A data block's hash isn't its leaf.
    BadBlock(u64),
    /// A parent isn't the hash of its children.
    BadTree,
    BadRoot,
    BadToc,
    /// A file's bytes don't match its TOC hash.
    BadFile,
}

impl FsiError {
    /// A short, plain description for the integrity screen.
    pub fn describe(&self) -> &'static str {
        match self {
            FsiError::TooShort => "The system image is shorter than its header.",
            FsiError::BadMagic => "This file isn’t a Freally system image.",
            FsiError::UnknownFormat(_) => "The system image’s format is newer than this bootloader.",
            FsiError::UnknownFlavour(_) => "The system image names a build flavour this bootloader doesn’t know.",
            FsiError::UnknownKey => "The system image was signed by a key this computer doesn’t trust.",
            FsiError::BadSignature => "The system image’s signature doesn’t match its contents.",
            FsiError::ReservedNotZero => "The system image’s header has been changed.",
            FsiError::BadLayout => "The system image’s layout doesn’t add up.",
            FsiError::BadBlock(_) => "A block of the system image has been changed or damaged.",
            FsiError::BadTree | FsiError::BadRoot => "The system image’s hash tree doesn’t match.",
            FsiError::BadToc => "The system image’s table of contents is damaged.",
            FsiError::BadFile => "A file in the system image doesn’t match its hash.",
        }
    }
}

/// The header's fields (block 0).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
    pub flavour: u16,
    pub version: [u8; 32],
    pub build_id: [u8; 20],
    pub created_unix: u64,
    pub image_len: u64,
    pub data_blocks: u64,
    pub toc_offset: u64,
    pub toc_len: u64,
    pub tree_offset: u64,
    pub tree_len: u64,
    pub merkle_root: [u8; 32],
    pub key_id: [u8; 8],
    pub signature: [u8; 64],
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

fn array_at<const N: usize>(b: &[u8], o: usize) -> [u8; N] {
    let mut a = [0u8; N];
    a.copy_from_slice(&b[o..o + N]);
    a
}

impl Header {
    /// Reads the header's fields. Checks only the magic and the format, so
    /// nothing else in it may be trusted until [`verify`] has checked the signature.
    pub fn parse(image: &[u8]) -> Result<Header, FsiError> {
        if image.len() < HEADER_LEN {
            return Err(FsiError::TooShort);
        }
        if image[0..8] != MAGIC {
            return Err(FsiError::BadMagic);
        }
        let format = u16_at(image, 8);
        if format != FORMAT {
            return Err(FsiError::UnknownFormat(format));
        }
        Ok(Header {
            flavour: u16_at(image, 10),
            version: array_at(image, 12),
            build_id: array_at(image, 44),
            created_unix: u64_at(image, 64),
            image_len: u64_at(image, 72),
            data_blocks: u64_at(image, 80),
            toc_offset: u64_at(image, 88),
            toc_len: u64_at(image, 96),
            tree_offset: u64_at(image, 104),
            tree_len: u64_at(image, 112),
            merkle_root: array_at(image, 120),
            key_id: array_at(image, 152),
            signature: array_at(image, SIGNED_LEN),
        })
    }

    /// Writes the header into `out` (reserved bytes zero).
    pub fn write(&self, out: &mut [u8; HEADER_LEN]) {
        out.fill(0);
        out[0..8].copy_from_slice(&MAGIC);
        out[8..10].copy_from_slice(&FORMAT.to_le_bytes());
        out[10..12].copy_from_slice(&self.flavour.to_le_bytes());
        out[12..44].copy_from_slice(&self.version);
        out[44..64].copy_from_slice(&self.build_id);
        for (o, v) in [
            (64, self.created_unix),
            (72, self.image_len),
            (80, self.data_blocks),
            (88, self.toc_offset),
            (96, self.toc_len),
            (104, self.tree_offset),
            (112, self.tree_len),
        ] {
            out[o..o + 8].copy_from_slice(&v.to_le_bytes());
        }
        out[120..152].copy_from_slice(&self.merkle_root);
        out[152..160].copy_from_slice(&self.key_id);
        out[SIGNED_LEN..].copy_from_slice(&self.signature);
    }

    /// The version as text (up to its first NUL).
    pub fn version_str(&self) -> &str {
        let end = self.version.iter().position(|&b| b == 0).unwrap_or(self.version.len());
        core::str::from_utf8(&self.version[..end]).unwrap_or("?")
    }
}

/// The first 8 bytes of SHA-256(public key).
pub fn key_id(public: &[u8; 32]) -> [u8; 8] {
    let h = sha256(public);
    let mut id = [0u8; 8];
    id.copy_from_slice(&h[..8]);
    id
}

/// An image that passed every check.
#[derive(Clone, Copy, Debug)]
pub struct Verified<'a> {
    pub header: Header,
    /// Which of the trusted keys signed it.
    pub key_index: usize,
    image: &'a [u8],
}

impl<'a> Verified<'a> {
    pub fn toc(&self) -> TocIter<'a> {
        let h = &self.header;
        TocIter::new(&self.image[h.toc_offset as usize..(h.toc_offset + h.toc_len) as usize])
    }

    /// The bytes of the file at `path`, if the image has one.
    pub fn file(&self, path: &str) -> Option<&'a [u8]> {
        let image = self.image;
        self.toc().find(|e| e.path() == path).map(|e| &image[e.offset as usize..(e.offset + e.len) as usize])
    }
}

/// Verifies `image` against the `trusted` public keys, in the order
/// `docs/fsi-format.md` gives. Nothing is skipped, ever.
pub fn verify<'a>(image: &'a [u8], trusted: &[[u8; 32]]) -> Result<Verified<'a>, FsiError> {
    let header = Header::parse(image)?;
    if header.flavour > 2 {
        return Err(FsiError::UnknownFlavour(header.flavour));
    }

    // 1. The signature, before any other field is trusted.
    let key_index = trusted.iter().position(|k| ct::eq(&key_id(k), &header.key_id)).ok_or(FsiError::UnknownKey)?;
    if !ed25519::verify(&trusted[key_index], &image[..SIGNED_LEN], &header.signature) {
        return Err(FsiError::BadSignature);
    }
    if image[160..SIGNED_LEN].iter().any(|&b| b != 0) {
        return Err(FsiError::ReservedNotZero);
    }

    // 2. The layout adds up.
    let block = BLOCK as u64;
    let h = &header;
    let layout_ok = (|| {
        let len = image.len() as u64;
        let data_end = 1u64.checked_add(h.data_blocks)?.checked_mul(block)?;
        let tree_len = tree_nodes(h.data_blocks).checked_mul(32)?;
        let ok = h.data_blocks >= 1
            && h.image_len == len
            && h.tree_offset == data_end
            && h.tree_len == tree_len
            && h.tree_offset.checked_add(h.tree_len)? == len
            && h.toc_offset == block
            && h.toc_len.is_multiple_of(TOC_ENTRY_LEN as u64)
            && h.toc_offset.checked_add(h.toc_len)? <= data_end;
        Some(ok)
    })();
    if layout_ok != Some(true) {
        return Err(FsiError::BadLayout);
    }

    // 3. Every block, every parent, the root.
    let data = &image[BLOCK..h.tree_offset as usize];
    let tree = &image[h.tree_offset as usize..];
    merkle::check(data, tree, &h.merkle_root)?;

    // 4. The TOC.
    toc::check(&image[h.toc_offset as usize..(h.toc_offset + h.toc_len) as usize], image.len() as u64, h.tree_offset)?;
    for e in TocIter::new(&image[h.toc_offset as usize..(h.toc_offset + h.toc_len) as usize]) {
        let bytes = &image[e.offset as usize..(e.offset + e.len) as usize];
        if !ct::eq(&sha256(bytes), &e.sha256) {
            return Err(FsiError::BadFile);
        }
    }

    Ok(Verified { header, key_index, image })
}

/// One file of an image the bootloader has already verified in full (the
/// signature, every Merkle block, every TOC hash), checked again on its own:
/// the header and TOC must add up, and the file's bytes must match its TOC
/// hash. For Freally Core, which starts conductor from that same image in
/// memory; this re-checks, it never stands in for `verify`.
pub fn recheck_file<'a>(image: &'a [u8], path: &str) -> Result<(TocEntry, &'a [u8]), FsiError> {
    let h = Header::parse(image)?;
    let toc_end = h.toc_offset.checked_add(h.toc_len).ok_or(FsiError::BadLayout)?;
    if toc_end > h.tree_offset || h.tree_offset > image.len() as u64 || !h.toc_len.is_multiple_of(TOC_ENTRY_LEN as u64)
    {
        return Err(FsiError::BadLayout);
    }
    let toc = &image[h.toc_offset as usize..toc_end as usize];
    toc::check(toc, image.len() as u64, h.tree_offset)?;
    let e = TocIter::new(toc).find(|e| e.path() == path).ok_or(FsiError::BadToc)?;
    let bytes = &image[e.offset as usize..(e.offset + e.len) as usize];
    if !ct::eq(&sha256(bytes), &e.sha256) {
        return Err(FsiError::BadFile);
    }
    Ok((e, bytes))
}

/// The TOC of an image the bootloader verified in full, its layout checked
/// again (each file's bytes are rechecked by `recheck_file` as it's used).
pub fn recheck_toc(image: &[u8]) -> Result<TocIter<'_>, FsiError> {
    let h = Header::parse(image)?;
    let toc_end = h.toc_offset.checked_add(h.toc_len).ok_or(FsiError::BadLayout)?;
    if toc_end > h.tree_offset || h.tree_offset > image.len() as u64 || !h.toc_len.is_multiple_of(TOC_ENTRY_LEN as u64)
    {
        return Err(FsiError::BadLayout);
    }
    let toc = &image[h.toc_offset as usize..toc_end as usize];
    toc::check(toc, image.len() as u64, h.tree_offset)?;
    Ok(TocIter::new(toc))
}
