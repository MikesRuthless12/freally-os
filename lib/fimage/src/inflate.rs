// SPDX-License-Identifier: Apache-2.0
//! DEFLATE (RFC 1951) and zlib (RFC 1950), decoding only (TASK-051): for
//! `xtask` reading the playbook's PNGs. Feature `decode`: never in the image,
//! which carries no decoder for any outside format (FR-025).
//!
//! A straightforward canonical-Huffman decoder, after Mark Adler's `puff`
//! (the reference, written from RFC 1951): every block type, every length
//! and distance code, every check the format has.

use alloc::vec::Vec;

const MAX_BITS: usize = 15;

struct Bits<'a> {
    data: &'a [u8],
    /// The next bit, counted from the start.
    at: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Result<u32, &'static str> {
        let byte = *self.data.get(self.at >> 3).ok_or("the deflate stream ends early")?;
        let b = (byte >> (self.at & 7)) & 1;
        self.at += 1;
        Ok(b as u32)
    }

    /// `n` bits, least significant first.
    fn bits(&mut self, n: u32) -> Result<u32, &'static str> {
        let mut v = 0;
        for i in 0..n {
            v |= self.bit()? << i;
        }
        Ok(v)
    }

    fn align(&mut self) {
        self.at = self.at.next_multiple_of(8);
    }
}

/// A canonical Huffman code: how many codes of each length, and the symbols
/// in code order.
struct Huffman {
    count: [u16; MAX_BITS + 1],
    symbol: Vec<u16>,
}

impl Huffman {
    /// From each symbol's code length (0: unused). An over-subscribed set is
    /// refused; an incomplete one is allowed (RFC 1951 permits a single code).
    fn new(lengths: &[u8]) -> Result<Huffman, &'static str> {
        let mut count = [0u16; MAX_BITS + 1];
        for &l in lengths {
            count[l as usize] += 1;
        }
        let mut left: i32 = 1;
        for &c in &count[1..] {
            left = (left << 1) - c as i32;
            if left < 0 {
                return Err("an over-subscribed Huffman code");
            }
        }
        let mut offs = [0u16; MAX_BITS + 1];
        for len in 1..MAX_BITS {
            offs[len + 1] = offs[len] + count[len];
        }
        let mut symbol = alloc::vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbol[offs[l as usize] as usize] = s as u16;
                offs[l as usize] += 1;
            }
        }
        Ok(Huffman { count, symbol })
    }

    fn decode(&self, b: &mut Bits) -> Result<u16, &'static str> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..=MAX_BITS {
            code |= b.bit()? as i32;
            let count = self.count[len] as i32;
            if code - count < first {
                return Ok(self.symbol[(index + code - first) as usize]);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err("a code no Huffman table has")
    }
}

const LEN_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
    8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] =
    [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

fn codes(b: &mut Bits, out: &mut Vec<u8>, lit: &Huffman, dist: &Huffman) -> Result<(), &'static str> {
    loop {
        let s = lit.decode(b)? as usize;
        if s < 256 {
            out.push(s as u8);
        } else if s == 256 {
            return Ok(());
        } else {
            let s = s - 257;
            if s >= 29 {
                return Err("a length code past 285");
            }
            let len = LEN_BASE[s] as usize + b.bits(LEN_EXTRA[s] as u32)? as usize;
            let d = dist.decode(b)? as usize;
            if d >= 30 {
                return Err("a distance code past 29");
            }
            let back = DIST_BASE[d] as usize + b.bits(DIST_EXTRA[d] as u32)? as usize;
            if back > out.len() {
                return Err("a distance before the start of the output");
            }
            let from = out.len() - back;
            for i in 0..len {
                let byte = out[from + i];
                out.push(byte);
            }
        }
    }
}

fn fixed() -> Result<(Huffman, Huffman), &'static str> {
    let mut l = [0u8; 288];
    l[..144].fill(8);
    l[144..256].fill(9);
    l[256..280].fill(7);
    l[280..].fill(8);
    Ok((Huffman::new(&l)?, Huffman::new(&[5u8; 30])?))
}

fn dynamic(b: &mut Bits) -> Result<(Huffman, Huffman), &'static str> {
    const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
    let nlen = b.bits(5)? as usize + 257;
    let ndist = b.bits(5)? as usize + 1;
    let ncode = b.bits(4)? as usize + 4;
    if nlen > 286 || ndist > 30 {
        return Err("too many length or distance codes");
    }
    let mut cl = [0u8; 19];
    for &i in &ORDER[..ncode] {
        cl[i] = b.bits(3)? as u8;
    }
    let lencode = Huffman::new(&cl)?;
    let mut lengths = [0u8; 316];
    let mut i = 0;
    while i < nlen + ndist {
        let s = lencode.decode(b)?;
        if s < 16 {
            lengths[i] = s as u8;
            i += 1;
            continue;
        }
        let (value, repeat) = match s {
            16 => (
                *lengths.get(i.wrapping_sub(1)).filter(|_| i > 0).ok_or("a repeat with nothing before it")?,
                3 + b.bits(2)?,
            ),
            17 => (0, 3 + b.bits(3)?),
            _ => (0, 11 + b.bits(7)?),
        };
        if i + repeat as usize > nlen + ndist {
            return Err("code lengths run past the end");
        }
        lengths[i..i + repeat as usize].fill(value);
        i += repeat as usize;
    }
    if lengths[256] == 0 {
        return Err("no end-of-block code");
    }
    Ok((Huffman::new(&lengths[..nlen])?, Huffman::new(&lengths[nlen..nlen + ndist])?))
}

/// The bytes a raw deflate stream holds; and how many bytes of it were used.
pub fn inflate(data: &[u8]) -> Result<(Vec<u8>, usize), &'static str> {
    let mut b = Bits { data, at: 0 };
    let mut out = Vec::new();
    loop {
        let last = b.bit()? == 1;
        match b.bits(2)? {
            0 => {
                b.align();
                let at = b.at >> 3;
                let head = data.get(at..at + 4).ok_or("a stored block's header is cut off")?;
                let len = u16::from_le_bytes([head[0], head[1]]);
                if len != !u16::from_le_bytes([head[2], head[3]]) {
                    return Err("a stored block's length doesn't match its complement");
                }
                let body = data.get(at + 4..at + 4 + len as usize).ok_or("a stored block is cut off")?;
                out.extend_from_slice(body);
                b.at = (at + 4 + len as usize) << 3;
            }
            1 => {
                let (lit, dist) = fixed()?;
                codes(&mut b, &mut out, &lit, &dist)?;
            }
            2 => {
                let (lit, dist) = dynamic(&mut b)?;
                codes(&mut b, &mut out, &lit, &dist)?;
            }
            _ => return Err("a deflate block of type 3"),
        }
        if last {
            return Ok((out, b.at.div_ceil(8)));
        }
    }
}

pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    b << 16 | a
}

/// A zlib stream's bytes: its header, the deflate data, and its Adler-32 checked.
pub fn zlib(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    let (&cmf, &flg) = (data.first().ok_or("an empty zlib stream")?, data.get(1).ok_or("a cut-off zlib header")?);
    if cmf & 0x0F != 8 || cmf >> 4 > 7 || (u16::from(cmf) << 8 | u16::from(flg)) % 31 != 0 || flg & 0x20 != 0 {
        return Err("a bad zlib header");
    }
    let (out, used) = inflate(&data[2..])?;
    let at = 2 + used;
    let sum = data.get(at..at + 4).ok_or("the zlib checksum is cut off")?;
    if u32::from_be_bytes([sum[0], sum[1], sum[2], sum[3]]) != adler32(&out) {
        return Err("the zlib checksum doesn't match");
    }
    Ok(out)
}
