// SPDX-License-Identifier: Apache-2.0
//! ChaCha20 (RFC 8439 § 2.3–2.4) and HChaCha20 (draft-irtf-cfrg-xchacha
//! § 2.2): 32-bit adds, rotations and xors only, so constant time by nature.

const SIGMA: [u32; 4] = [0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574];

fn quarter(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

/// The 20 rounds (10 column + diagonal double rounds), in place.
fn rounds(s: &mut [u32; 16]) {
    for _ in 0..10 {
        quarter(s, 0, 4, 8, 12);
        quarter(s, 1, 5, 9, 13);
        quarter(s, 2, 6, 10, 14);
        quarter(s, 3, 7, 11, 15);
        quarter(s, 0, 5, 10, 15);
        quarter(s, 1, 6, 11, 12);
        quarter(s, 2, 7, 8, 13);
        quarter(s, 3, 4, 9, 14);
    }
}

fn words<const N: usize>(b: &[u8]) -> [u32; N] {
    core::array::from_fn(|i| u32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]))
}

fn state(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u32; 16] {
    let k: [u32; 8] = words(key);
    let n: [u32; 3] = words(nonce);
    [SIGMA[0], SIGMA[1], SIGMA[2], SIGMA[3], k[0], k[1], k[2], k[3], k[4], k[5], k[6], k[7], counter, n[0], n[1], n[2]]
}

/// One 64-byte block of key stream (§ 2.3).
pub fn block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let init = state(key, counter, nonce);
    let mut s = init;
    rounds(&mut s);
    let mut out = [0u8; 64];
    for i in 0..16 {
        out[4 * i..4 * i + 4].copy_from_slice(&s[i].wrapping_add(init[i]).to_le_bytes());
    }
    out
}

/// Encrypts (or decrypts) `data` in place, the key stream starting at block
/// `counter` (§ 2.4). The counter wraps after 2^32 blocks (256 GiB): callers
/// keep messages far shorter.
pub fn xor(key: &[u8; 32], counter: u32, nonce: &[u8; 12], data: &mut [u8]) {
    for (i, chunk) in data.chunks_mut(64).enumerate() {
        let ks = block(key, counter.wrapping_add(i as u32), nonce);
        for (b, k) in chunk.iter_mut().zip(ks) {
            *b ^= k;
        }
    }
}

/// HChaCha20: a 256-bit subkey from a key and a 128-bit nonce (XChaCha's
/// first step).
pub fn hchacha20(key: &[u8; 32], nonce: &[u8; 16]) -> [u8; 32] {
    let k: [u32; 8] = words(key);
    let n: [u32; 4] = words(nonce);
    let mut s = [
        SIGMA[0], SIGMA[1], SIGMA[2], SIGMA[3], k[0], k[1], k[2], k[3], k[4], k[5], k[6], k[7], n[0], n[1], n[2], n[3],
    ];
    rounds(&mut s);
    let mut out = [0u8; 32];
    for (i, w) in s[..4].iter().chain(&s[12..]).enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&w.to_le_bytes());
    }
    out
}
