// SPDX-License-Identifier: Apache-2.0
//! Deflate (RFC 1951) for the Brain's screenshots (TASK-081): LZ77 over a
//! 32 KiB window, greedy, in one block of the fixed Huffman codes (§ 3.2.6).
//! A screen of flat colours shrinks 20 to 100 times, which is what crosses a
//! serial line; it never needs the decoder's dynamic tables.

use alloc::vec::Vec;

const WINDOW: usize = 32 * 1024;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
/// How many earlier places with the same three bytes are tried.
const CHAIN: usize = 32;
const HASH_BITS: u32 = 15;

const LEN_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
    8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] =
    [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    /// `n` bits of `v`, least significant first (headers and extra bits).
    fn put(&mut self, v: u32, n: u32) {
        self.acc |= (v as u64) << self.n;
        self.n += n;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// A Huffman code, most significant bit first (§ 3.1.1).
    fn code(&mut self, code: u32, len: u32) {
        self.put(code.reverse_bits() >> (32 - len), len);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

/// The fixed literal/length code for symbol `s` (0..=287).
fn lit(b: &mut Bits, s: u32) {
    match s {
        0..=143 => b.code(0x30 + s, 8),
        144..=255 => b.code(0x190 + s - 144, 9),
        256..=279 => b.code(s - 256, 7),
        _ => b.code(0xC0 + s - 280, 8),
    }
}

fn length(b: &mut Bits, len: usize) {
    let i = LEN_BASE.iter().rposition(|&base| base as usize <= len).unwrap_or(0);
    lit(b, 257 + i as u32);
    b.put((len - LEN_BASE[i] as usize) as u32, LEN_EXTRA[i] as u32);
}

fn distance(b: &mut Bits, d: usize) {
    let i = DIST_BASE.iter().rposition(|&base| base as usize <= d).unwrap_or(0);
    b.code(i as u32, 5);
    b.put((d - DIST_BASE[i] as usize) as u32, DIST_EXTRA[i] as u32);
}

fn hash(d: &[u8], i: usize) -> usize {
    let v = (d[i] as u32) << 16 | (d[i + 1] as u32) << 8 | d[i + 2] as u32;
    (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
}

/// `data` as a raw deflate stream (no zlib header).
pub fn compress(data: &[u8]) -> Vec<u8> {
    let mut b = Bits { out: Vec::with_capacity(data.len() / 8 + 64), acc: 0, n: 0 };
    b.put(1, 1); // BFINAL
    b.put(1, 2); // BTYPE 01: fixed Huffman codes
    // head[h]: the last place + 1 with that hash (0: none); prev: the chain.
    let mut head = alloc::vec![0u32; 1 << HASH_BITS];
    let mut prev = alloc::vec![0u32; WINDOW];
    let insert = |head: &mut [u32], prev: &mut [u32], i: usize| {
        if i + MIN_MATCH <= data.len() {
            let h = hash(data, i);
            prev[i % WINDOW] = head[h];
            head[h] = i as u32 + 1;
        }
    };
    let mut i = 0;
    while i < data.len() {
        let mut best = (0, 0);
        if i + MIN_MATCH <= data.len() {
            let mut cand = head[hash(data, i)];
            let max = MAX_MATCH.min(data.len() - i);
            for _ in 0..CHAIN {
                if cand == 0 {
                    break;
                }
                let j = cand as usize - 1;
                if i - j > WINDOW {
                    break;
                }
                let n = data[j..].iter().zip(&data[i..i + max]).take_while(|(a, c)| a == c).count();
                if n > best.0 {
                    best = (n, i - j);
                    if n == max {
                        break;
                    }
                }
                let next = prev[j % WINDOW];
                // An older place, or the chain has wrapped round the window.
                if next == 0 || next as usize > j {
                    break;
                }
                cand = next;
            }
        }
        if best.0 >= MIN_MATCH {
            length(&mut b, best.0);
            distance(&mut b, best.1);
            for k in i..i + best.0 {
                insert(&mut head, &mut prev, k);
            }
            i += best.0;
        } else {
            lit(&mut b, data[i] as u32);
            insert(&mut head, &mut prev, i);
            i += 1;
        }
    }
    lit(&mut b, 256);
    b.finish()
}

#[cfg(all(test, feature = "decode"))]
mod tests {
    use super::compress;
    use crate::inflate::inflate;
    use alloc::vec::Vec;

    fn round(data: &[u8]) -> usize {
        let z = compress(data);
        let (back, used) = inflate(&z).unwrap();
        assert_eq!(back, data);
        assert_eq!(used, z.len());
        z.len()
    }

    #[test]
    fn what_goes_in_comes_out() {
        round(b"");
        round(b"a");
        round(b"abcabcabcabcabcabcabcabcabcabc and then something else entirely");
        // Every byte value, and a long run (lengths up to 258, then more).
        let all: Vec<u8> = (0..=255u8).collect();
        round(&all);
        round(&[7u8; 100_000]);
        // Noise: a linear congruential stream, nothing to match.
        let mut x = 1u32;
        let noise: Vec<u8> = (0..70_000)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                (x >> 16) as u8
            })
            .collect();
        round(&noise);
        // Matches reaching back across the whole window.
        let mut far = noise[..40_000].to_vec();
        far.extend_from_slice(&noise[100..9_000]);
        round(&far);
    }

    #[test]
    fn a_screen_of_flat_colour_shrinks() {
        // 640 x 400 RGB: a dark ground with a lighter panel.
        let mut px = Vec::new();
        for y in 0..400 {
            for x in 0..640 {
                let c = if (100..300).contains(&x) && (50..200).contains(&y) {
                    [0x2a, 0x2e, 0x35]
                } else {
                    [0x12, 0x14, 0x18]
                };
                px.extend_from_slice(&c);
            }
        }
        let n = round(&px);
        assert!(n * 50 < px.len(), "{n} bytes from {}", px.len());
    }
}
