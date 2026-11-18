// SPDX-License-Identifier: Apache-2.0
//! felf: reads a static x86-64 ELF executable's loadable segments (no_std, no
//! allocation). Everything is bounds-checked; a segment that is both writable
//! and executable is refused (W^X). Used by Freally Core to start conductor and
//! by conductor to start services.
#![no_std]

/// Segment permission bits (`p_flags`).
pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

const PT_LOAD: u32 = 1;
const PAGE: u64 = 4096;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ElfError {
    /// Not a 64-bit little-endian x86-64 executable.
    NotAnExecutable,
    /// A header or segment points outside the file, or doesn't add up.
    Damaged,
    /// A segment is both writable and executable.
    WritableAndExecutable,
    /// A segment lies outside the range the caller allows.
    OutOfRange,
    /// More segments than a program may have (16).
    TooManySegments,
}

impl ElfError {
    pub fn describe(self) -> &'static str {
        match self {
            ElfError::NotAnExecutable => "not an x86-64 executable",
            ElfError::Damaged => "a damaged program file",
            ElfError::WritableAndExecutable => "a segment that's both writable and executable",
            ElfError::OutOfRange => "a segment outside its allowed range",
            ElfError::TooManySegments => "too many segments",
        }
    }
}

/// One `PT_LOAD` segment.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Segment {
    pub vaddr: u64,
    pub memsz: u64,
    /// Where its bytes start in the file, and how many (the rest of `memsz` is zero).
    pub offset: u64,
    pub filesz: u64,
    pub flags: u32,
}

impl Segment {
    /// The page-aligned range the segment covers.
    pub fn pages(&self) -> (u64, u64) {
        (self.vaddr & !(PAGE - 1), (self.vaddr + self.memsz).next_multiple_of(PAGE))
    }
    pub fn writable(&self) -> bool {
        self.flags & PF_W != 0
    }
    pub fn executable(&self) -> bool {
        self.flags & PF_X != 0
    }
}

/// A checked executable.
#[derive(Clone, Copy, Debug)]
pub struct Elf<'a> {
    pub entry: u64,
    bytes: &'a [u8],
    segments: [Segment; 16],
    count: usize,
}

fn u16_at(b: &[u8], o: usize) -> Result<u16, ElfError> {
    Ok(u16::from_le_bytes(b.get(o..o + 2).ok_or(ElfError::Damaged)?.try_into().map_err(|_| ElfError::Damaged)?))
}
fn u32_at(b: &[u8], o: usize) -> Result<u32, ElfError> {
    Ok(u32::from_le_bytes(b.get(o..o + 4).ok_or(ElfError::Damaged)?.try_into().map_err(|_| ElfError::Damaged)?))
}
fn u64_at(b: &[u8], o: usize) -> Result<u64, ElfError> {
    Ok(u64::from_le_bytes(b.get(o..o + 8).ok_or(ElfError::Damaged)?.try_into().map_err(|_| ElfError::Damaged)?))
}

impl<'a> Elf<'a> {
    /// Parses `bytes`; every segment must lie in `[lo, hi)`, and the entry
    /// point inside an executable segment. Segments may not overlap.
    pub fn parse(bytes: &'a [u8], lo: u64, hi: u64) -> Result<Elf<'a>, ElfError> {
        if bytes.get(0..4) != Some(b"\x7FELF") || bytes.get(4) != Some(&2) || bytes.get(5) != Some(&1) {
            return Err(ElfError::NotAnExecutable);
        }
        if u16_at(bytes, 16)? != 2 || u16_at(bytes, 18)? != 0x3E {
            return Err(ElfError::NotAnExecutable); // ET_EXEC, x86-64
        }
        let entry = u64_at(bytes, 24)?;
        let phoff = u64_at(bytes, 32)? as usize;
        let phentsize = u16_at(bytes, 54)? as usize;
        let phnum = u16_at(bytes, 56)? as usize;
        if phentsize < 56 || phnum > 64 {
            return Err(ElfError::Damaged);
        }
        let empty = Segment { vaddr: 0, memsz: 0, offset: 0, filesz: 0, flags: 0 };
        let mut segments = [empty; 16];
        let mut count = 0;
        for i in 0..phnum {
            let h = phoff.checked_add(i * phentsize).ok_or(ElfError::Damaged)?;
            if u32_at(bytes, h)? != PT_LOAD {
                continue;
            }
            let s = Segment {
                flags: u32_at(bytes, h + 4)?,
                offset: u64_at(bytes, h + 8)?,
                vaddr: u64_at(bytes, h + 16)?,
                filesz: u64_at(bytes, h + 32)?,
                memsz: u64_at(bytes, h + 40)?,
            };
            if s.memsz == 0 {
                continue;
            }
            if s.writable() && s.executable() {
                return Err(ElfError::WritableAndExecutable);
            }
            if s.filesz > s.memsz || s.offset.checked_add(s.filesz).is_none_or(|e| e > bytes.len() as u64) {
                return Err(ElfError::Damaged);
            }
            let end = s.vaddr.checked_add(s.memsz).ok_or(ElfError::Damaged)?;
            if s.vaddr < lo || end > hi || end.checked_next_multiple_of(PAGE).is_none() {
                return Err(ElfError::OutOfRange);
            }
            // Two segments may not share a page: each page gets one set of permissions.
            let (a0, a1) = s.pages();
            if segments[..count].iter().any(|o| {
                let (b0, b1) = o.pages();
                a0 < b1 && b0 < a1
            }) {
                return Err(ElfError::Damaged);
            }
            if count == segments.len() {
                return Err(ElfError::TooManySegments);
            }
            segments[count] = s;
            count += 1;
        }
        if count == 0
            || !segments[..count].iter().any(|s| s.executable() && entry >= s.vaddr && entry < s.vaddr + s.memsz)
        {
            return Err(ElfError::Damaged);
        }
        Ok(Elf { entry, bytes, segments, count })
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments[..self.count]
    }

    /// A segment's bytes in the file (`filesz` of them).
    pub fn data(&self, s: &Segment) -> &'a [u8] {
        &self.bytes[s.offset as usize..(s.offset + s.filesz) as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    /// A minimal ELF with the given (flags, vaddr, filesz, memsz) segments and entry.
    fn elf(segs: &[(u32, u64, u64, u64)], entry: u64) -> Vec<u8> {
        let phoff = 64usize;
        let data_at = phoff + 56 * segs.len();
        let mut b = std::vec![0u8; data_at];
        b[0..4].copy_from_slice(b"\x7FELF");
        b[4] = 2;
        b[5] = 1;
        b[6] = 1;
        b[16..18].copy_from_slice(&2u16.to_le_bytes());
        b[18..20].copy_from_slice(&0x3Eu16.to_le_bytes());
        b[24..32].copy_from_slice(&entry.to_le_bytes());
        b[32..40].copy_from_slice(&(phoff as u64).to_le_bytes());
        b[54..56].copy_from_slice(&56u16.to_le_bytes());
        b[56..58].copy_from_slice(&(segs.len() as u16).to_le_bytes());
        let mut off = data_at as u64;
        for (i, &(flags, vaddr, filesz, memsz)) in segs.iter().enumerate() {
            let h = phoff + 56 * i;
            b[h..h + 4].copy_from_slice(&1u32.to_le_bytes());
            b[h + 4..h + 8].copy_from_slice(&flags.to_le_bytes());
            b[h + 8..h + 16].copy_from_slice(&off.to_le_bytes());
            b[h + 16..h + 24].copy_from_slice(&vaddr.to_le_bytes());
            b[h + 32..h + 40].copy_from_slice(&filesz.to_le_bytes());
            b[h + 40..h + 48].copy_from_slice(&memsz.to_le_bytes());
            off += filesz;
        }
        let total = off as usize;
        b.resize(total, 0xAB);
        b
    }

    const LO: u64 = 0x40_0000;
    const HI: u64 = 0x8000_0000_0000;

    #[test]
    fn a_text_and_data_program_parses() {
        let b = elf(&[(PF_R | PF_X, 0x40_1000, 100, 100), (PF_R | PF_W, 0x40_3000, 10, 5000)], 0x40_1010);
        let e = Elf::parse(&b, LO, HI).unwrap();
        assert_eq!(e.entry, 0x40_1010);
        assert_eq!(e.segments().len(), 2);
        assert_eq!(e.data(&e.segments()[0]).len(), 100);
        assert_eq!(e.segments()[1].pages(), (0x40_3000, 0x40_5000));
    }

    #[test]
    fn writable_and_executable_is_refused() {
        let b = elf(&[(PF_R | PF_W | PF_X, 0x40_1000, 100, 100)], 0x40_1000);
        assert_eq!(Elf::parse(&b, LO, HI).unwrap_err(), ElfError::WritableAndExecutable);
    }

    #[test]
    fn segments_outside_the_range_are_refused() {
        let b = elf(&[(PF_R | PF_X, 0x1000, 100, 100)], 0x1000);
        assert_eq!(Elf::parse(&b, LO, HI).unwrap_err(), ElfError::OutOfRange);
        let b = elf(&[(PF_R | PF_X, 0xFFFF_FFFF_8000_0000, 100, 100)], 0xFFFF_FFFF_8000_0000);
        assert_eq!(Elf::parse(&b, LO, HI).unwrap_err(), ElfError::OutOfRange);
    }

    #[test]
    fn damaged_files_are_refused() {
        let good = elf(&[(PF_R | PF_X, 0x40_1000, 100, 100)], 0x40_1000);
        // Truncated anywhere: never a panic, always an error.
        for n in 0..good.len() {
            assert!(Elf::parse(&good[..n], LO, HI).is_err(), "truncated at {n}");
        }
        // filesz past the end of the file.
        let mut b = good.clone();
        b[64 + 32..64 + 40].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(Elf::parse(&b, LO, HI).is_err());
        // An entry point outside every executable segment.
        let b = elf(&[(PF_R | PF_X, 0x40_1000, 100, 100)], 0x50_0000);
        assert_eq!(Elf::parse(&b, LO, HI).unwrap_err(), ElfError::Damaged);
        // Overlapping pages.
        let b = elf(&[(PF_R | PF_X, 0x40_1000, 100, 100), (PF_R | PF_W, 0x40_1800, 10, 10)], 0x40_1000);
        assert_eq!(Elf::parse(&b, LO, HI).unwrap_err(), ElfError::Damaged);
        // Not ELF at all.
        assert_eq!(Elf::parse(b"MZ\x90\x00", LO, HI).unwrap_err(), ElfError::NotAnExecutable);
    }
}
