// SPDX-License-Identifier: Apache-2.0
//! Constant-time helpers: no branch and no memory access depends on secret data.

/// `true` if `a == b`. The time depends only on the lengths, never on the contents.
#[inline(never)]
pub fn eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    core::hint::black_box(diff) == 0
}

/// All ones if `bit` is 1, all zeros if it's 0. Only the low bit of `bit` counts.
#[inline(always)]
pub fn mask64(bit: u64) -> u64 {
    0u64.wrapping_sub(core::hint::black_box(bit) & 1)
}

/// `a` if `bit` is 0, `b` if `bit` is 1, without a branch.
#[inline(always)]
pub fn select64(bit: u64, a: u64, b: u64) -> u64 {
    let m = mask64(bit);
    a ^ (m & (a ^ b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eq_compares_contents_and_lengths() {
        assert!(eq(b"freally", b"freally"));
        assert!(!eq(b"freally", b"freallz"));
        assert!(!eq(b"freally", b"freall"));
        assert!(eq(b"", b""));
    }

    #[test]
    fn select_picks_by_bit() {
        assert_eq!(select64(0, 7, 9), 7);
        assert_eq!(select64(1, 7, 9), 9);
    }
}
