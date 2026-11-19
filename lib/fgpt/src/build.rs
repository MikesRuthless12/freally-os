// SPDX-License-Identifier: Apache-2.0
//! Writing a GPT (feature `build`: `cargo xtask image --gpt`): the protective
//! MBR, the primary header and entries at the disk's start, the backup
//! entries and header at its end.

use crate::{ENTRIES, ENTRY_SIZE, HEADER_SIZE, NAME_UNITS, REVISION, SIGNATURE, crc32, guid_bytes};
use alloc::vec;
use alloc::vec::Vec;
use fcore_abi::Guid;

/// A partition to write: its type, its unique GUID, its first and last
/// (inclusive) LBA, and its name.
pub struct Part<'a> {
    pub type_guid: Guid,
    pub unique: Guid,
    pub first_lba: u64,
    pub last_lba: u64,
    pub name: &'a str,
}

/// The tables of a disk of `blocks` blocks of `block` bytes: the bytes for
/// its start (LBA 0 to the end of the primary entries) and for its end (the
/// backup entries, then the backup header in the last block).
pub fn tables(blocks: u64, block: u32, disk_guid: Guid, parts: &[Part]) -> (Vec<u8>, Vec<u8>) {
    let bs = block as usize;
    let table_blocks = (ENTRIES * ENTRY_SIZE).div_ceil(block) as u64;
    let last = blocks - 1;
    let (first_usable, last_usable) = (2 + table_blocks, last - 1 - table_blocks);
    let mut entries = vec![0u8; (ENTRIES * ENTRY_SIZE) as usize];
    for (e, p) in entries.as_chunks_mut::<{ ENTRY_SIZE as usize }>().0.iter_mut().zip(parts) {
        e[..16].copy_from_slice(&guid_bytes(&p.type_guid));
        e[16..32].copy_from_slice(&guid_bytes(&p.unique));
        e[32..40].copy_from_slice(&p.first_lba.to_le_bytes());
        e[40..48].copy_from_slice(&p.last_lba.to_le_bytes());
        for (i, u) in p.name.encode_utf16().take(NAME_UNITS).enumerate() {
            e[56 + 2 * i..58 + 2 * i].copy_from_slice(&u.to_le_bytes());
        }
    }
    let entries_crc = crc32(&entries);
    let header = |my: u64, alternate: u64, entries_lba: u64| {
        let mut h = vec![0u8; bs];
        h[..8].copy_from_slice(&SIGNATURE);
        h[8..12].copy_from_slice(&REVISION.to_le_bytes());
        h[12..16].copy_from_slice(&HEADER_SIZE.to_le_bytes());
        h[24..32].copy_from_slice(&my.to_le_bytes());
        h[32..40].copy_from_slice(&alternate.to_le_bytes());
        h[40..48].copy_from_slice(&first_usable.to_le_bytes());
        h[48..56].copy_from_slice(&last_usable.to_le_bytes());
        h[56..72].copy_from_slice(&guid_bytes(&disk_guid));
        h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
        h[80..84].copy_from_slice(&ENTRIES.to_le_bytes());
        h[84..88].copy_from_slice(&ENTRY_SIZE.to_le_bytes());
        h[88..92].copy_from_slice(&entries_crc.to_le_bytes());
        let crc = crc32(&h[..HEADER_SIZE as usize]);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
        h
    };
    // The protective MBR: one partition of type 0xEE over the whole disk.
    let mut mbr = vec![0u8; bs];
    let p = &mut mbr[446..462];
    p[1..4].copy_from_slice(&[0x00, 0x02, 0x00]);
    p[4] = 0xEE;
    p[5..8].copy_from_slice(&[0xFF, 0xFF, 0xFF]);
    p[8..12].copy_from_slice(&1u32.to_le_bytes());
    p[12..16].copy_from_slice(&(last.min(u32::MAX as u64) as u32).to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    let mut start = mbr;
    start.extend(header(1, last, 2));
    start.extend(&entries);
    start.resize(first_usable as usize * bs, 0);
    let mut end = entries.clone();
    end.resize(table_blocks as usize * bs, 0);
    end.extend(header(last, 1, last - table_blocks));
    (start, end)
}

/// A GUID made from 16 bytes (a hash of what names it, so a build's disk is
/// the same every time), marked as a random (version 4, variant 1) GUID.
pub fn guid_from(b: [u8; 16]) -> Guid {
    let mut d4 = [0u8; 8];
    d4.copy_from_slice(&b[8..]);
    d4[0] = (d4[0] & 0x3F) | 0x80;
    let d3 = (u16::from_le_bytes([b[6], b[7]]) & 0x0FFF) | 0x4000;
    Guid::new(u32::from_le_bytes([b[0], b[1], b[2], b[3]]), u16::from_le_bytes([b[4], b[5]]), d3, d4)
}
