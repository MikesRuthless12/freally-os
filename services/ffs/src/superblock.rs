// SPDX-License-Identifier: Apache-2.0
//! The superblock (§ 4): two slots, blocks 0 and 1; generation `g` is
//! written to slot `g % 2`, so a crash while one is written leaves the other
//! whole. Mount takes the valid slot with the higher generation.

use crate::ptr::BlockPtr;
use crate::{BLOCK, FsError};
use fcrypto::crc32c::crc32c;

pub const MAGIC: [u8; 8] = *b"FRLYFS01";
pub const VERSION: u16 = 1;
/// `flags` bit 0: the last session unmounted cleanly.
pub const CLEAN: u16 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Superblock {
    pub flags: u16,
    pub generation: u64,
    pub block_count: u64,
    pub uuid: [u8; 16],
    pub root_tree: BlockPtr,
    pub free_tree: BlockPtr,
    pub ref_tree: BlockPtr,
    pub used_blocks: u64,
    pub next_subvol: u64,
}

impl Superblock {
    /// The slot generation `g` goes to.
    pub fn slot(generation: u64) -> u64 {
        generation % 2
    }

    pub fn encode(&self) -> [u8; BLOCK] {
        let mut b = [0u8; BLOCK];
        b[..8].copy_from_slice(&MAGIC);
        b[8..10].copy_from_slice(&VERSION.to_le_bytes());
        b[10..12].copy_from_slice(&self.flags.to_le_bytes());
        b[16..24].copy_from_slice(&self.generation.to_le_bytes());
        b[24..32].copy_from_slice(&self.block_count.to_le_bytes());
        b[32..48].copy_from_slice(&self.uuid);
        b[48..80].copy_from_slice(&self.root_tree.bytes());
        b[80..112].copy_from_slice(&self.free_tree.bytes());
        b[112..144].copy_from_slice(&self.ref_tree.bytes());
        b[144..152].copy_from_slice(&self.used_blocks.to_le_bytes());
        b[152..160].copy_from_slice(&self.next_subvol.to_le_bytes());
        let crc = crc32c(&b[..4092]);
        b[4092..].copy_from_slice(&crc.to_le_bytes());
        b
    }

    /// A slot's superblock if it's valid: the magic, the version, the CRC,
    /// and a size that fits a partition of `blocks`.
    pub fn decode(b: &[u8; BLOCK], blocks: u64) -> Option<Superblock> {
        let u64_at = |o: usize| {
            let mut a = [0u8; 8];
            a.copy_from_slice(&b[o..o + 8]);
            u64::from_le_bytes(a)
        };
        let crc = u32::from_le_bytes([b[4092], b[4093], b[4094], b[4095]]);
        if b[..8] != MAGIC || u16::from_le_bytes([b[8], b[9]]) != VERSION || crc32c(&b[..4092]) != crc {
            return None;
        }
        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(&b[32..48]);
        let sb = Superblock {
            flags: u16::from_le_bytes([b[10], b[11]]),
            generation: u64_at(16),
            block_count: u64_at(24),
            uuid,
            root_tree: BlockPtr::from_bytes(&b[48..80]),
            free_tree: BlockPtr::from_bytes(&b[80..112]),
            ref_tree: BlockPtr::from_bytes(&b[112..144]),
            used_blocks: u64_at(144),
            next_subvol: u64_at(152),
        };
        (sb.generation >= 1 && sb.block_count <= blocks && sb.block_count > crate::FIRST_FREE).then_some(sb)
    }

    /// The newer of two slots' superblocks, as mount takes it.
    pub fn newest(a: Option<Superblock>, b: Option<Superblock>) -> Result<Superblock, FsError> {
        match (a, b) {
            (Some(x), Some(y)) => Ok(if x.generation >= y.generation { x } else { y }),
            (Some(x), None) | (None, Some(x)) => Ok(x),
            (None, None) => Err(FsError::NotFreallyFs),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_damage() {
        let sb = Superblock { generation: 7, block_count: 1000, uuid: [3; 16], next_subvol: 257, ..Default::default() };
        let b = sb.encode();
        assert_eq!(Superblock::decode(&b, 1000), Some(sb));
        assert_eq!(Superblock::decode(&b, 999), None, "larger than the partition");
        let mut torn = b;
        torn[100] ^= 1;
        assert_eq!(Superblock::decode(&torn, 1000), None, "a torn write fails its CRC");
        assert_eq!(Superblock::decode(&[0; BLOCK], 1000), None);
        let older = Superblock { generation: 6, ..sb };
        assert_eq!(Superblock::newest(Some(older), Some(sb)).unwrap().generation, 7);
        assert_eq!(Superblock::newest(None, Some(older)).unwrap().generation, 6);
        assert_eq!(Superblock::newest(None, None), Err(FsError::NotFreallyFs));
        assert_eq!((Superblock::slot(7), Superblock::slot(8)), (1, 0));
    }
}
