// SPDX-License-Identifier: Apache-2.0
//! The GUID Partition Table (UEFI 2.10 § 5.3), written from the
//! specification: read by the bootloader (TASK-091) and by drv-nvme's
//! partition interlock (TASK-092), written by `cargo xtask image --gpt`; and
//! Freally OS's own layout (PRD § 3 *Boot medium layouts*): the ESP, two
//! system slots, the data partition.
//!
//! Reading takes no allocator: the caller reads the header's block and then
//! the entries' blocks, and [`Header::parse`] and [`Table::new`] check them
//! (signatures, both CRC32s, the header's own LBA, the entries inside the
//! usable area). A damaged primary table is the caller's cue to read the
//! backup at the disk's last block.
#![no_std]

#[cfg(feature = "build")]
extern crate alloc;

#[cfg(feature = "build")]
pub mod build;

use fcore_abi::Guid;

/// The EFI system partition's type: C12A7328-F81F-11D2-BA4B-00A0C93EC93B.
pub const PART_TYPE_ESP: Guid =
    Guid::new(0xC12A_7328, 0xF81F, 0x11D2, [0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B]);
pub use fcore_abi::{PART_TYPE_FREALLY_DATA, PART_TYPE_FREALLY_SYSTEM};

/// "EFI PART".
pub const SIGNATURE: [u8; 8] = *b"EFI PART";
/// Revision 1.0.
pub const REVISION: u32 = 0x0001_0000;
/// The header's size (the rest of its block is zero).
pub const HEADER_SIZE: u32 = 92;
/// An entry's size, and how many a table holds (16 KiB of them).
pub const ENTRY_SIZE: u32 = 128;
pub const ENTRIES: u32 = 128;
/// The partitions' names, in UTF-16 code units.
pub const NAME_UNITS: usize = 36;

/// What's wrong with a table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GptError {
    /// No "EFI PART" (not a GPT disk).
    NotGpt,
    /// A revision, size or field this reader doesn't take.
    Unsupported,
    /// A CRC32 that doesn't match: the header's or the entries'.
    BadCrc,
    /// The header isn't where it says it is, or its areas don't fit the disk.
    BadLayout,
}

/// CRC-32 (IEEE 802.3, reflected, as GPT and PNG use it).
pub fn crc32(data: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut t = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
                k += 1;
            }
            t[i] = c;
            i += 1;
        }
        t
    };
    !data.iter().fold(!0u32, |c, &b| TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8))
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

/// A GUID from its 16 bytes on disk (mixed-endian, as UEFI stores it).
pub fn guid_at(b: &[u8], o: usize) -> Guid {
    let mut d4 = [0u8; 8];
    d4.copy_from_slice(&b[o + 8..o + 16]);
    Guid::new(u32_at(b, o), u16::from_le_bytes([b[o + 4], b[o + 5]]), u16::from_le_bytes([b[o + 6], b[o + 7]]), d4)
}

/// A GUID's 16 bytes on disk.
pub fn guid_bytes(g: &Guid) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[..4].copy_from_slice(&g.data1.to_le_bytes());
    out[4..6].copy_from_slice(&g.data2.to_le_bytes());
    out[6..8].copy_from_slice(&g.data3.to_le_bytes());
    out[8..].copy_from_slice(&g.data4);
    out
}

/// A table's header (LBA 1, or the backup at the disk's last LBA).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub my_lba: u64,
    pub alternate_lba: u64,
    pub first_usable: u64,
    pub last_usable: u64,
    pub disk_guid: Guid,
    pub entries_lba: u64,
    pub entry_count: u32,
    pub entry_size: u32,
    pub entries_crc: u32,
}

impl Header {
    /// Checks the header in `block` (one block, read from `lba` of a disk
    /// whose last LBA is `last_lba`): its signature, revision, size, CRC32,
    /// that it says it's at `lba`, and that its areas fit the disk.
    pub fn parse(block: &[u8], lba: u64, last_lba: u64) -> Result<Header, GptError> {
        if block.len() < HEADER_SIZE as usize || block[..8] != SIGNATURE {
            return Err(GptError::NotGpt);
        }
        let size = u32_at(block, 12);
        if u32_at(block, 8) != REVISION || size < HEADER_SIZE || size as usize > block.len() {
            return Err(GptError::Unsupported);
        }
        let mut copy = [0u8; 512];
        let n = (size as usize).min(512);
        copy[..n].copy_from_slice(&block[..n]);
        copy[16..20].fill(0);
        if crc32(&copy[..n]) != u32_at(block, 16) {
            return Err(GptError::BadCrc);
        }
        let h = Header {
            my_lba: u64_at(block, 24),
            alternate_lba: u64_at(block, 32),
            first_usable: u64_at(block, 40),
            last_usable: u64_at(block, 48),
            disk_guid: guid_at(block, 56),
            entries_lba: u64_at(block, 72),
            entry_count: u32_at(block, 80),
            entry_size: u32_at(block, 84),
            entries_crc: u32_at(block, 88),
        };
        let entries_ok = h.entry_size >= ENTRY_SIZE && h.entry_size.is_power_of_two() && h.entry_count <= 1024;
        let fits = h.my_lba == lba
            && h.first_usable <= h.last_usable
            && h.last_usable < last_lba
            && h.entries_lba > 0
            && h.entries_lba <= last_lba;
        if !entries_ok {
            return Err(GptError::Unsupported);
        }
        if !fits {
            return Err(GptError::BadLayout);
        }
        Ok(h)
    }

    /// The entries' length in bytes (read that many from `entries_lba`).
    pub fn entries_len(&self) -> usize {
        self.entry_count as usize * self.entry_size as usize
    }
}

/// One partition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Partition {
    pub type_guid: Guid,
    pub unique: Guid,
    pub first_lba: u64,
    /// Inclusive.
    pub last_lba: u64,
    pub attributes: u64,
    pub name: [u16; NAME_UNITS],
}

impl Partition {
    /// Its blocks.
    pub fn blocks(&self) -> u64 {
        self.last_lba - self.first_lba + 1
    }

    /// Whether `name` is its name.
    pub fn named(&self, name: &str) -> bool {
        let len = self.name.iter().position(|&u| u == 0).unwrap_or(NAME_UNITS);
        name.encode_utf16().eq(self.name[..len].iter().copied())
    }
}

/// A checked table: its header and its entries.
#[derive(Clone, Copy, Debug)]
pub struct Table<'a> {
    pub header: Header,
    entries: &'a [u8],
}

impl<'a> Table<'a> {
    /// Checks `entries` (the header's [`entries_len`](Header::entries_len)
    /// bytes from its `entries_lba`) against the header's CRC32, and that
    /// every partition lies inside the usable area.
    pub fn new(header: Header, entries: &'a [u8]) -> Result<Table<'a>, GptError> {
        let entries = entries.get(..header.entries_len()).ok_or(GptError::BadLayout)?;
        if crc32(entries) != header.entries_crc {
            return Err(GptError::BadCrc);
        }
        let t = Table { header, entries };
        for p in t.partitions() {
            if p.first_lba > p.last_lba || p.first_lba < header.first_usable || p.last_lba > header.last_usable {
                return Err(GptError::BadLayout);
            }
        }
        Ok(t)
    }

    /// Every partition in use, in the table's order.
    pub fn partitions(&self) -> impl Iterator<Item = Partition> + '_ {
        self.entries.chunks_exact(self.header.entry_size as usize).filter_map(|e| {
            let type_guid = guid_at(e, 0);
            if type_guid == Guid::new(0, 0, 0, [0; 8]) {
                return None;
            }
            let mut name = [0u16; NAME_UNITS];
            for (i, u) in name.iter_mut().enumerate() {
                *u = u16::from_le_bytes([e[56 + 2 * i], e[57 + 2 * i]]);
            }
            Some(Partition {
                type_guid,
                unique: guid_at(e, 16),
                first_lba: u64_at(e, 32),
                last_lba: u64_at(e, 40),
                attributes: u64_at(e, 48),
                name,
            })
        })
    }

    /// The partition whose unique GUID is `unique`.
    pub fn find(&self, unique: &Guid) -> Option<Partition> {
        self.partitions().find(|p| p.unique == *unique)
    }

    /// Freally OS's system slots, A then B: the partitions named
    /// `FREALLY_SYSTEM_A` and `FREALLY_SYSTEM_B` of the system type.
    pub fn system_slots(&self) -> [Option<Partition>; 2] {
        let slot = |name: &str| self.partitions().find(|p| p.type_guid == PART_TYPE_FREALLY_SYSTEM && p.named(name));
        [slot("FREALLY_SYSTEM_A"), slot("FREALLY_SYSTEM_B")]
    }

    /// Whether `[lba, lba + count)` lies wholly inside one partition of a
    /// Freally type (system or data): the partition interlock's question
    /// (PRD § 9: writes anywhere else are refused).
    pub fn freally_owns(&self, lba: u64, count: u64) -> bool {
        let Some(end) = lba.checked_add(count) else { return false };
        count > 0
            && self.partitions().any(|p| {
                (p.type_guid == PART_TYPE_FREALLY_SYSTEM || p.type_guid == PART_TYPE_FREALLY_DATA)
                    && lba >= p.first_lba
                    && end <= p.last_lba + 1
            })
    }
}

/// A partition of a layout: its type, its name, its first and last (inclusive) LBA.
pub type Planned = (Guid, &'static str, u64, u64);

/// Freally OS's layout (PRD § 3) on a disk of `blocks` blocks of `block`
/// bytes: the ESP (64 MiB), FREALLY_SYSTEM_A and _B (512 MiB each), and
/// FREALLY_DATA (the rest), each starting on a whole MiB. `None` when the
/// disk can't hold the three and 64 MiB of data.
pub fn freally_layout(blocks: u64, block: u32) -> Option<[Planned; 4]> {
    let mib = (1 << 20) / block as u64;
    let table_blocks = (ENTRIES * ENTRY_SIZE).div_ceil(block) as u64;
    // The backup table at the end: its entries, then its header.
    let last_usable = blocks.checked_sub(2 + table_blocks)?;
    let esp = (mib, mib + 64 * mib - 1);
    let a = (esp.1 + 1, esp.1 + 512 * mib);
    let b = (a.1 + 1, a.1 + 512 * mib);
    let data_first = b.1 + 1;
    let data_last = (last_usable + 1) / mib * mib - 1;
    if data_last < data_first + 64 * mib - 1 {
        return None;
    }
    Some([
        (PART_TYPE_ESP, "EFI system partition", esp.0, esp.1),
        (PART_TYPE_FREALLY_SYSTEM, "FREALLY_SYSTEM_A", a.0, a.1),
        (PART_TYPE_FREALLY_SYSTEM, "FREALLY_SYSTEM_B", b.0, b.1),
        (PART_TYPE_FREALLY_DATA, "FREALLY_DATA", data_first, data_last),
    ])
}

/// A Freally disk made bigger (TASK-114: a tester's virtual disk grown
/// before its first boot): what to write so FREALLY_DATA, its last
/// partition, takes the new room (up to a whole MiB).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grown {
    /// The primary header (LBA 1) and the backup (at `backup_lba`).
    pub primary: [u8; HEADER_SIZE as usize],
    pub backup: [u8; HEADER_SIZE as usize],
    pub backup_lba: u64,
    /// Where the backup's copy of the entries goes.
    pub backup_entries_lba: u64,
    /// FREALLY_DATA's new last LBA.
    pub data_last: u64,
}

/// The table in `header` (LBA 1's block) and `entries` grown to a disk of
/// `blocks` blocks of `block` bytes: `entries` changed in place (the data
/// partition's last LBA). `None` when there's nothing to grow (the backup
/// header is already at the disk's end) or it isn't Freally OS's to grow
/// (no valid table, or its last partition isn't a FREALLY_DATA that
/// reached the old end).
pub fn grow(header: &[u8], entries: &mut [u8], blocks: u64, block: u32) -> Option<Grown> {
    let last = blocks.checked_sub(1)?;
    let h = Header::parse(header, 1, last).ok()?;
    if h.alternate_lba >= last || u32_at(header, 12) != HEADER_SIZE {
        return None;
    }
    Table::new(h, entries).ok()?;
    let size = h.entry_size as usize;
    let (at, old_last) = (0..h.entry_count as usize)
        .filter(|i| guid_at(entries, i * size) != Guid::new(0, 0, 0, [0; 8]))
        .map(|i| (i, u64_at(entries, i * size + 40)))
        .max_by_key(|&(_, l)| l)?;
    let mib = (1u64 << 20) / block as u64;
    if guid_at(entries, at * size) != PART_TYPE_FREALLY_DATA || old_last + mib <= h.last_usable {
        return None;
    }
    let table_blocks = (h.entry_count * h.entry_size).div_ceil(block) as u64;
    let last_usable = last.checked_sub(1 + table_blocks)?;
    let data_last = (last_usable + 1) / mib * mib - 1;
    if data_last <= old_last {
        return None;
    }
    entries[at * size + 40..at * size + 48].copy_from_slice(&data_last.to_le_bytes());
    let entries_crc = crc32(&entries[..h.entries_len()]);
    let mut primary = [0u8; HEADER_SIZE as usize];
    primary.copy_from_slice(&header[..HEADER_SIZE as usize]);
    primary[32..40].copy_from_slice(&last.to_le_bytes());
    primary[48..56].copy_from_slice(&last_usable.to_le_bytes());
    primary[88..92].copy_from_slice(&entries_crc.to_le_bytes());
    let mut backup = primary;
    backup[24..32].copy_from_slice(&last.to_le_bytes());
    backup[32..40].copy_from_slice(&1u64.to_le_bytes());
    let backup_entries_lba = last - table_blocks;
    backup[72..80].copy_from_slice(&backup_entries_lba.to_le_bytes());
    for hd in [&mut primary, &mut backup] {
        hd[16..20].fill(0);
        let crc = crc32(&hd[..]);
        hd[16..20].copy_from_slice(&crc.to_le_bytes());
    }
    Some(Grown { primary, backup, backup_lba: last, backup_entries_lba, data_last })
}
