// SPDX-License-Identifier: Apache-2.0
//! SHA-256 and SHA-512 (FIPS 180-4).

/// SHA-512's round constants (FIPS 180-4 § 4.2.3): the first 64 bits of the
/// fractional parts of the cube roots of the first 80 primes.
#[rustfmt::skip]
const K512: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc,
    0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118,
    0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694,
    0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
    0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
    0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2, 0xd5a79147930aa725, 0x06ca6351e003826f, 0x142929670a0e6e70,
    0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
    0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
    0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30,
    0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b,
    0xca273eceea26619c, 0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178,
    0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
    0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

/// SHA-256's round constants are the first 32 bits of the same cube roots
/// (FIPS 180-4 § 4.2.2), so they're the high halves of SHA-512's first 64.
const K256: [u32; 64] = {
    let mut k = [0u32; 64];
    let mut i = 0;
    while i < 64 {
        k[i] = (K512[i] >> 32) as u32;
        i += 1;
    }
    k
};

#[rustfmt::skip]
const IV256: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

#[rustfmt::skip]
const IV512: [u64; 8] = [
    0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1,
    0x510e527fade682d1, 0x9b05688c2b3e6c1f, 0x1f83d9abfb41bd6b, 0x5be0cd19137e2179,
];

/// A running SHA-256.
#[derive(Clone)]
pub struct Sha256 {
    h: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    total: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub const fn new() -> Sha256 {
        Sha256 { h: IV256, buf: [0; 64], buf_len: 0, total: 0 }
    }

    fn compress(h: &mut [u32; 8], block: &[u8]) {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([block[4 * i], block[4 * i + 1], block[4 * i + 2], block[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K256[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }

    /// Whole blocks, with the SHA extensions when the CPU has them (the
    /// `shani` feature, TASK-076), else in software.
    fn compress_blocks(h: &mut [u32; 8], blocks: &[[u8; 64]]) {
        #[cfg(all(target_arch = "x86_64", feature = "shani"))]
        if crate::sha256_ni::available() {
            // SAFETY: `available` asked CPUID for every extension it uses.
            unsafe { crate::sha256_ni::compress(h, blocks, &K256) };
            return;
        }
        for block in blocks {
            Self::compress(h, block);
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        if self.buf_len > 0 {
            let take = (64 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len < 64 {
                return;
            }
            let block = self.buf;
            Self::compress_blocks(&mut self.h, &[block]);
            self.buf_len = 0;
        }
        let (blocks, rest) = data.as_chunks::<64>();
        Self::compress_blocks(&mut self.h, blocks);
        self.buf[..rest.len()].copy_from_slice(rest);
        self.buf_len = rest.len();
    }

    pub fn finish(mut self) -> [u8; 32] {
        let bits = self.total.wrapping_mul(8);
        let mut pad = [0u8; 72];
        pad[0] = 0x80;
        let pad_len = if self.buf_len < 56 { 56 - self.buf_len } else { 120 - self.buf_len };
        self.update(&pad[..pad_len]);
        self.update(&bits.to_be_bytes());
        debug_assert_eq!(self.buf_len, 0);
        let mut out = [0u8; 32];
        for (i, word) in self.h.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

/// A running SHA-512.
#[derive(Clone)]
pub struct Sha512 {
    h: [u64; 8],
    buf: [u8; 128],
    buf_len: usize,
    total: u128,
}

impl Default for Sha512 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha512 {
    pub const fn new() -> Sha512 {
        Sha512 { h: IV512, buf: [0; 128], buf_len: 0, total: 0 }
    }

    fn compress(h: &mut [u64; 8], block: &[u8]) {
        let mut w = [0u64; 80];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            let mut b = [0u8; 8];
            b.copy_from_slice(&block[8 * i..8 * i + 8]);
            *word = u64::from_be_bytes(b);
        }
        for i in 16..80 {
            let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
            let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
        for i in 0..80 {
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K512[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u128);
        if self.buf_len > 0 {
            let take = (128 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len < 128 {
                return;
            }
            let block = self.buf;
            Self::compress(&mut self.h, &block);
            self.buf_len = 0;
        }
        let (blocks, rest) = data.as_chunks::<128>();
        for block in blocks {
            Self::compress(&mut self.h, block);
        }
        self.buf[..rest.len()].copy_from_slice(rest);
        self.buf_len = rest.len();
    }

    pub fn finish(mut self) -> [u8; 64] {
        let bits = self.total.wrapping_mul(8);
        let mut pad = [0u8; 144];
        pad[0] = 0x80;
        let pad_len = if self.buf_len < 112 { 112 - self.buf_len } else { 240 - self.buf_len };
        self.update(&pad[..pad_len]);
        self.update(&bits.to_be_bytes());
        debug_assert_eq!(self.buf_len, 0);
        let mut out = [0u8; 64];
        for (i, word) in self.h.iter().enumerate() {
            out[8 * i..8 * i + 8].copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

/// SHA-256 of `data`.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finish()
}

/// SHA-512 of `data`.
pub fn sha512(data: &[u8]) -> [u8; 64] {
    let mut h = Sha512::new();
    h.update(data);
    h.finish()
}

#[cfg(all(test, target_arch = "x86_64", feature = "shani"))]
mod shani_tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    /// The SHA extensions give the software path's state, block for block,
    /// on every length of a pseudo-random message up to 4 KiB (TASK-076).
    #[test]
    fn the_sha_extensions_match_the_software_path() {
        if !crate::sha256_ni::available() {
            std::println!("SKIP: this CPU has no SHA extensions");
            return;
        }
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let data: Vec<u8> = (0..4096)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        let (blocks, _) = data.as_chunks::<64>();
        for n in 0..=blocks.len() {
            let mut soft = IV256;
            for b in &blocks[..n] {
                Sha256::compress(&mut soft, b);
            }
            let mut ni = IV256;
            // SAFETY: `available` said yes.
            unsafe { crate::sha256_ni::compress(&mut ni, &blocks[..n], &K256) };
            assert_eq!(ni, soft, "after {n} blocks");
        }
        // And whole digests at every length, through the public path.
        for len in [0, 1, 55, 56, 63, 64, 65, 127, 128, 1000, 4096] {
            let mut soft = Sha256::new();
            soft.total = len as u64;
            let ni = sha256(&data[..len]);
            let (b, rest) = data[..len].as_chunks::<64>();
            for block in b {
                Sha256::compress(&mut soft.h, block);
            }
            soft.buf[..rest.len()].copy_from_slice(rest);
            soft.buf_len = rest.len();
            // Finish in software too: pad by hand.
            let bits = (len as u64) * 8;
            let mut tail = rest.to_vec();
            tail.push(0x80);
            while tail.len() % 64 != 56 {
                tail.push(0);
            }
            tail.extend_from_slice(&bits.to_be_bytes());
            let mut h = soft.h;
            for block in tail.as_chunks::<64>().0 {
                Sha256::compress(&mut h, block);
            }
            let want: Vec<u8> = h.iter().flat_map(|w| w.to_be_bytes()).collect();
            assert_eq!(ni.to_vec(), want, "sha256 of {len} bytes");
        }
    }
}
