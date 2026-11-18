// SPDX-License-Identifier: Apache-2.0
//! The Merkle tree: leaves are SHA-256 of each data block; parents are
//! SHA-256(left ‖ right); an odd node is promoted unchanged; level 0 first.

use crate::{BLOCK, FsiError};
use fcrypto::{ct, sha2::Sha256, sha2::sha256};

/// How many nodes a tree over `leaves` leaves has, root included.
pub fn tree_nodes(leaves: u64) -> u64 {
    if leaves == 0 {
        return 0;
    }
    let mut n = leaves;
    let mut total = n;
    while n > 1 {
        n = n.div_ceil(2);
        total += n;
    }
    total
}

/// The hash of one data block (a short last block is zero-padded).
pub(crate) fn leaf(block: &[u8]) -> [u8; 32] {
    if block.len() == BLOCK {
        sha256(block)
    } else {
        let mut h = Sha256::new();
        h.update(block);
        let pad = [0u8; BLOCK];
        h.update(&pad[..BLOCK - block.len()]);
        h.finish()
    }
}

pub(crate) fn parent(left: &[u8], right: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(left);
    h.update(right);
    h.finish()
}

/// Checks the stored `tree` against the `data` blocks and the signed `root`.
pub(crate) fn check(data: &[u8], tree: &[u8], root: &[u8; 32]) -> Result<(), FsiError> {
    let leaves = data.len().div_ceil(BLOCK);
    for (i, block) in data.chunks(BLOCK).enumerate() {
        if !ct::eq(&leaf(block), &tree[32 * i..32 * i + 32]) {
            return Err(FsiError::BadBlock(i as u64 + 1));
        }
    }
    let mut level_start = 0;
    let mut n = leaves;
    while n > 1 {
        let next_start = level_start + n;
        let m = n.div_ceil(2);
        for j in 0..m {
            let l = level_start + 2 * j;
            let expected = if 2 * j + 1 < n {
                parent(&tree[32 * l..32 * l + 32], &tree[32 * (l + 1)..32 * (l + 1) + 32])
            } else {
                let mut a = [0u8; 32];
                a.copy_from_slice(&tree[32 * l..32 * l + 32]);
                a
            };
            let at = 32 * (next_start + j);
            if !ct::eq(&expected, &tree[at..at + 32]) {
                return Err(FsiError::BadTree);
            }
        }
        level_start = next_start;
        n = m;
    }
    if !ct::eq(&tree[32 * level_start..32 * level_start + 32], root) {
        return Err(FsiError::BadRoot);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_counts() {
        assert_eq!(tree_nodes(1), 1);
        assert_eq!(tree_nodes(2), 3);
        assert_eq!(tree_nodes(3), 6); // 3 + 2 + 1
        assert_eq!(tree_nodes(5), 11); // 5 + 3 + 2 + 1
    }
}
