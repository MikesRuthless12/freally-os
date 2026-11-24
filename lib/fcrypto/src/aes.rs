// SPDX-License-Identifier: Apache-2.0
//! AES-128 and AES-256 encryption (FIPS 197), for AES-GCM ([`crate::gcm`]):
//! GCM only ever encrypts blocks, so there's no decryption here.
//!
//! **Constant time, no tables:** the S-box is computed, not looked up (a
//! table indexed by key or data leaks both through the cache): the inverse
//! in GF(2^8) as x^254 by a fixed chain of multiplications, each multiply
//! branch-free, then the affine map. It's slow (a few microseconds a block);
//! the AES-NI path comes with TASK-178's hardware step.

/// The round keys, 11 (AES-128) or 15 (AES-256) of 16 bytes.
#[derive(Clone)]
pub struct Aes {
    keys: [[u8; 16]; 15],
    rounds: usize,
}

impl core::fmt::Debug for Aes {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Aes { .. }")
    }
}

/// a · b in GF(2^8) mod x^8 + x^4 + x^3 + x + 1, without a branch on either.
fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0u8;
    for _ in 0..8 {
        p ^= a & 0u8.wrapping_sub(b & 1);
        let hi = 0u8.wrapping_sub(a >> 7);
        a = (a << 1) ^ (0x1B & hi);
        b >>= 1;
    }
    p
}

/// The S-box (FIPS 197 § 5.1.1): the inverse (0 for 0) as x^254, then the affine map.
fn sbox(x: u8) -> u8 {
    // 254 = 0b1111_1110: square and multiply, every step taken whatever x is.
    let mut r = 1u8;
    for bit in (0..8).rev() {
        r = gf_mul(r, r);
        let m = gf_mul(r, x);
        let take = 0u8.wrapping_sub((254u8 >> bit) & 1);
        r = (m & take) | (r & !take);
    }
    r ^ r.rotate_left(1) ^ r.rotate_left(2) ^ r.rotate_left(3) ^ r.rotate_left(4) ^ 0x63
}

impl Aes {
    /// The key schedule (FIPS 197 § 5.2) of a 16- or 32-byte key; `None` for any other length.
    pub fn new(key: &[u8]) -> Option<Aes> {
        let nk = match key.len() {
            16 => 4,
            32 => 8,
            _ => return None,
        };
        let rounds = nk + 6;
        let mut w = [[0u8; 4]; 60];
        for (i, word) in key.chunks_exact(4).enumerate() {
            w[i].copy_from_slice(word);
        }
        let mut rcon = 1u8;
        for i in nk..4 * (rounds + 1) {
            let mut t = w[i - 1];
            if i % nk == 0 {
                t = [sbox(t[1]) ^ rcon, sbox(t[2]), sbox(t[3]), sbox(t[0])];
                rcon = gf_mul(rcon, 2);
            } else if nk > 6 && i % nk == 4 {
                t = t.map(sbox);
            }
            for j in 0..4 {
                w[i][j] = w[i - nk][j] ^ t[j];
            }
        }
        let mut keys = [[0u8; 16]; 15];
        for (r, k) in keys.iter_mut().enumerate().take(rounds + 1) {
            for j in 0..4 {
                k[4 * j..4 * j + 4].copy_from_slice(&w[4 * r + j]);
            }
        }
        Some(Aes { keys, rounds })
    }

    /// Encrypts one block (FIPS 197 § 5.1).
    pub fn encrypt(&self, block: &[u8; 16]) -> [u8; 16] {
        let mut s = *block;
        xor(&mut s, &self.keys[0]);
        for r in 1..=self.rounds {
            // SubBytes and ShiftRows together: byte (row, column) comes from column + row.
            let mut t = [0u8; 16];
            for c in 0..4 {
                for row in 0..4 {
                    t[4 * c + row] = sbox(s[4 * ((c + row) % 4) + row]);
                }
            }
            if r < self.rounds {
                for c in 0..4 {
                    let col = [t[4 * c], t[4 * c + 1], t[4 * c + 2], t[4 * c + 3]];
                    for row in 0..4 {
                        t[4 * c + row] = gf_mul(col[row], 2)
                            ^ gf_mul(col[(row + 1) % 4], 3)
                            ^ col[(row + 2) % 4]
                            ^ col[(row + 3) % 4];
                    }
                }
            }
            s = t;
            xor(&mut s, &self.keys[r]);
        }
        s
    }
}

fn xor(a: &mut [u8; 16], b: &[u8; 16]) {
    for (x, y) in a.iter_mut().zip(b) {
        *x ^= y;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sbox_is_fips_197s() {
        // Figure 7's corners and a few inside.
        for (x, y) in [(0x00, 0x63), (0x01, 0x7C), (0x53, 0xED), (0xFF, 0x16), (0x10, 0xCA), (0xC9, 0xDD)] {
            assert_eq!(sbox(x), y, "S({x:#04x})");
        }
        // A permutation of all 256 bytes.
        let mut seen = [false; 256];
        for x in 0..=255u8 {
            seen[sbox(x) as usize] = true;
        }
        assert!(seen.iter().all(|&s| s));
    }

    #[test]
    fn keys_of_other_lengths_are_refused() {
        assert!(Aes::new(&[0; 24]).is_none(), "AES-192 isn't needed (TLS 1.3 has no suite for it)");
        assert!(Aes::new(&[0; 15]).is_none());
    }
}
