// SPDX-License-Identifier: Apache-2.0
//! updater (TASK-110, FR-050): an update package (`.fup`, PRD § 3) taken
//! from Settings a piece at a time and written to the slot not running.
//!
//! - Nothing is written until the package's header checks out: signed by a
//!   key this system's bootloader trusts, for this flavour, a version that
//!   isn't the running one, the notes as signed.
//! - The slot's first block (the image's signed header) is cleared first and
//!   written last: a slot written part-way holds no image the bootloader
//!   would take.
//! - The pieces are hashed as they come; at the end the image must match
//!   the hash the package's header signed, its own header must be signed by
//!   a trusted key and name the same version, and what was written must read
//!   back the same. Only then is the first block written, and conductor asked
//!   for the trial.
#![no_std]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use fcrypto::sha2::Sha256;
use fsi::fup::{self, FupError, HEADER, Head};

pub const BLOCK: usize = 4096;
/// Blocks a disk call (the block protocol's most).
pub const RUN: usize = 15;
/// The most bytes a piece carries.
pub const PIECE: usize = 60 * 1024;

/// The A/B record's states for a slot (`fbootctl`), as the updater reports them.
pub const EMPTY: u8 = 0;
pub const GOOD: u8 = 1;

/// The state to report for the slot not running: the record's, unless the
/// slot's first block holds no image (an install cleared it and didn't
/// finish), which makes it empty: nothing to switch back to.
pub fn other_state(recorded: u8, slot: &mut dyn Slot) -> u8 {
    match slot.read(0, 1) {
        Ok(b) if b.starts_with(&fsi::MAGIC) => recorded,
        _ => EMPTY,
    }
}

/// The slot not running: whole blocks from its partition's start.
pub trait Slot {
    /// Its size in blocks.
    fn blocks(&self) -> u64;
    /// Whole blocks from `block`, at most `RUN`.
    fn write(&mut self, block: u64, bytes: &[u8]) -> Result<(), String>;
    fn read(&mut self, block: u64, count: u32) -> Result<Vec<u8>, String>;
    fn flush(&mut self) -> Result<(), String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    Fup(FupError),
    /// Bigger than the slot, or more bytes than the header said.
    TooBig,
    /// A piece not where the last one ended.
    OutOfOrder,
    /// Finished before every byte came.
    NotAllThere,
    /// The disk said no, or what was written didn't read back the same.
    Disk(String),
}

impl From<FupError> for Refusal {
    fn from(e: FupError) -> Refusal {
        Refusal::Fup(e)
    }
}

impl Refusal {
    pub fn describe(&self) -> String {
        match self {
            Refusal::Fup(e) => e.describe().to_string(),
            Refusal::TooBig => String::from("This update is bigger than the space for it."),
            Refusal::OutOfOrder | Refusal::NotAllThere => String::from("The update didn't arrive whole: try again."),
            Refusal::Disk(e) => alloc::format!("The update couldn't be written: {e}"),
        }
    }
}

/// A package's header and notes checked (`bytes`: its first 256 bytes and
/// the notes, at least): what it is and what it says.
pub fn inspect(bytes: &[u8], trusted: &[[u8; 32]], flavour: u8, running: &str) -> Result<(Head, String), Refusal> {
    let h = fup::head(bytes, trusted, flavour)?;
    let n = h.notes_len as usize;
    let notes = bytes.get(HEADER..HEADER + n).ok_or(Refusal::NotAllThere)?;
    let notes = String::from(fup::notes(&h, notes)?);
    fup::may_install(&h, running)?;
    Ok((h, notes))
}

/// An install under way.
pub struct Install {
    pub head: Head,
    pub notes: String,
    trusted: Vec<[u8; 32]>,
    received: u64,
    hash: Sha256,
    /// Bytes not yet a whole run (or block, at the end).
    tail: Vec<u8>,
    /// The image's first block, held back until the end.
    first: Option<Vec<u8>>,
    /// The next block to write.
    next: u64,
}

impl Install {
    /// The header and notes checked, the slot's first block cleared.
    pub fn begin(
        head_and_notes: &[u8],
        trusted: &[[u8; 32]],
        flavour: u8,
        running: &str,
        slot: &mut dyn Slot,
    ) -> Result<Install, Refusal> {
        let (head, notes) = inspect(head_and_notes, trusted, flavour, running)?;
        if head.image_len.div_ceil(BLOCK as u64) > slot.blocks() || head.image_len < BLOCK as u64 {
            return Err(Refusal::TooBig);
        }
        slot.write(0, &[0u8; BLOCK]).map_err(Refusal::Disk)?;
        slot.flush().map_err(Refusal::Disk)?;
        Ok(Install {
            head,
            notes,
            trusted: trusted.to_vec(),
            received: 0,
            hash: Sha256::new(),
            tail: Vec::new(),
            first: None,
            next: 1,
        })
    }

    /// How much of the image has come.
    pub fn received(&self) -> u64 {
        self.received
    }

    /// The image's bytes from `at`, in order.
    pub fn piece(&mut self, at: u64, bytes: &[u8], slot: &mut dyn Slot) -> Result<(), Refusal> {
        if at != self.received {
            return Err(Refusal::OutOfOrder);
        }
        if bytes.len() > PIECE || self.received + bytes.len() as u64 > self.head.image_len {
            return Err(Refusal::TooBig);
        }
        self.hash.update(bytes);
        self.received += bytes.len() as u64;
        self.tail.extend_from_slice(bytes);
        self.drain(slot, false)
    }

    /// Whole runs out of the tail (and, at the end, what's left, padded).
    fn drain(&mut self, slot: &mut dyn Slot, end: bool) -> Result<(), Refusal> {
        if self.first.is_none() && self.tail.len() >= BLOCK {
            self.first = Some(self.tail.drain(..BLOCK).collect());
        }
        if end && !self.tail.len().is_multiple_of(BLOCK) {
            self.tail.resize(self.tail.len().next_multiple_of(BLOCK), 0);
        }
        while self.first.is_some() {
            let whole = self.tail.len() / BLOCK;
            let n = whole.min(RUN);
            if n == 0 || (n < RUN && !end) {
                break;
            }
            slot.write(self.next, &self.tail[..n * BLOCK]).map_err(Refusal::Disk)?;
            self.tail.drain(..n * BLOCK);
            self.next += n as u64;
        }
        Ok(())
    }

    /// The image whole: its hash as signed, its own header's signature, and
    /// what was written read back the same; then its first block written.
    /// The version to put on trial.
    pub fn finish(mut self, slot: &mut dyn Slot) -> Result<String, Refusal> {
        if self.received != self.head.image_len {
            return Err(Refusal::NotAllThere);
        }
        let hash = core::mem::replace(&mut self.hash, Sha256::new()).finish();
        if !fcrypto::ct::eq(&hash, &self.head.image_hash) {
            return Err(Refusal::Fup(FupError::Changed));
        }
        self.drain(slot, true)?;
        let first = self.first.take().ok_or(Refusal::NotAllThere)?;
        fup::check_image_head(&self.head, &first, &self.trusted)?;
        slot.flush().map_err(Refusal::Disk)?;
        // Read back: the first block from here, the rest from the disk.
        let len = self.head.image_len;
        let mut back = Sha256::new();
        back.update(&first);
        let (mut block, mut left) = (1u64, len - BLOCK as u64);
        while left > 0 {
            let n = left.div_ceil(BLOCK as u64).min(RUN as u64) as u32;
            let bytes = slot.read(block, n).map_err(Refusal::Disk)?;
            let take = (left as usize).min(bytes.len());
            back.update(&bytes[..take]);
            left -= take as u64;
            block += n as u64;
        }
        if !fcrypto::ct::eq(&back.finish(), &self.head.image_hash) {
            return Err(Refusal::Disk(String::from("what was written didn't read back the same")));
        }
        slot.write(0, &first).map_err(Refusal::Disk)?;
        slot.flush().map_err(Refusal::Disk)?;
        Ok(String::from(self.head.version()))
    }
}

#[cfg(test)]
mod tests;
