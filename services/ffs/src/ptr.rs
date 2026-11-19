// SPDX-License-Identifier: Apache-2.0
//! Block pointers (§ 3): where a block is, the generation that wrote it,
//! and its checksum, so a lost, torn, stale or misdirected write is caught
//! when it's read, never trusted.

use crate::BLOCK;
use fcrypto::crc32c::crc32c;

/// 32 bytes on disk: `block`, `generation`, `check` (plain: the CRC32C in
/// its first 4 bytes; encrypted, TASK-098: the Poly1305 tag).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub struct BlockPtr {
    pub block: u64,
    pub generation: u64,
    pub check: [u8; 16],
}

impl BlockPtr {
    pub const LEN: usize = 32;

    pub fn bytes(&self) -> [u8; 32] {
        let mut b = [0u8; 32];
        b[..8].copy_from_slice(&self.block.to_le_bytes());
        b[8..16].copy_from_slice(&self.generation.to_le_bytes());
        b[16..].copy_from_slice(&self.check);
        b
    }

    pub fn from_bytes(b: &[u8]) -> BlockPtr {
        let mut block = [0u8; 8];
        let mut generation = [0u8; 8];
        let mut check = [0u8; 16];
        block.copy_from_slice(&b[..8]);
        generation.copy_from_slice(&b[8..16]);
        check.copy_from_slice(&b[16..32]);
        BlockPtr { block: u64::from_le_bytes(block), generation: u64::from_le_bytes(generation), check }
    }

    /// The plain check of `data`.
    pub fn plain_check(data: &[u8; BLOCK]) -> [u8; 16] {
        let mut c = [0u8; 16];
        c[..4].copy_from_slice(&crc32c(data).to_le_bytes());
        c
    }

    /// Whether `data` is the block this pointer names (plain).
    pub fn matches(&self, data: &[u8; BLOCK]) -> bool {
        Self::plain_check(data) == self.check
    }
}
