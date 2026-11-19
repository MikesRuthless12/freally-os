// SPDX-License-Identifier: Apache-2.0
//! PNG (ISO/IEC 15948, RFC 1950/1951 for the zlib stream), 8-bit RGB or RGBA,
//! filter 0 on every row, deflate in stored blocks. Larger than a compressed PNG,
//! but exact, quick and small to write. The decoder reads stored-block PNGs only.

use alloc::vec::Vec;

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// CRC-32 (ISO-HDLC, reflected 0xEDB88320), as PNG chunks use.
const CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

pub fn crc32(parts: &[&[u8]]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for part in parts {
        for &b in *part {
            c = CRC_TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
        }
    }
    c ^ 0xFFFF_FFFF
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
}

/// Encodes `w × h` pixels of `channels` bytes each (3 RGB, 4 RGBA), row by row.
pub fn encode(w: u32, h: u32, channels: u8, pixels: &[u8]) -> Vec<u8> {
    encode_with(w, h, channels, pixels, false)
}

/// As [`encode`], compressed ([`crate::deflate`]): for a picture that has to
/// cross a slow line (the Brain's screenshots).
pub fn encode_deflated(w: u32, h: u32, channels: u8, pixels: &[u8]) -> Vec<u8> {
    encode_with(w, h, channels, pixels, true)
}

/// `raw` in stored deflate blocks.
fn stored(z: &mut Vec<u8>, raw: &[u8]) {
    z.reserve(raw.len() + raw.len() / 65535 * 5 + 16);
    let mut blocks = raw.chunks(65535).peekable();
    if blocks.peek().is_none() {
        z.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(b) = blocks.next() {
        z.push(if blocks.peek().is_none() { 1 } else { 0 });
        let len = b.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(b);
    }
}

fn encode_with(w: u32, h: u32, channels: u8, pixels: &[u8], deflated: bool) -> Vec<u8> {
    assert!(channels == 3 || channels == 4, "RGB or RGBA");
    let row = w as usize * channels as usize;
    assert_eq!(pixels.len(), row * h as usize, "pixel count");
    let mut raw = Vec::with_capacity((row + 1) * h as usize);
    for r in pixels.chunks(row) {
        raw.push(0); // filter: none
        raw.extend_from_slice(r);
    }
    let mut z = alloc::vec![0x78, 0x01]; // deflate, 32 KiB window, no dictionary
    if deflated {
        z.extend_from_slice(&crate::deflate::compress(&raw));
    } else {
        stored(&mut z, &raw);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, if channels == 4 { 6 } else { 2 }, 0, 0, 0]);
    let mut out = Vec::with_capacity(z.len() + 64);
    out.extend_from_slice(&SIGNATURE);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// A decoded image: `w × h`, `channels` bytes per pixel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    pub w: u32,
    pub h: u32,
    pub channels: u8,
    pub pixels: Vec<u8>,
}

/// The header's facts and the joined IDAT data of an 8-bit RGB or RGBA,
/// non-interlaced PNG, every chunk's CRC checked.
fn chunks(png: &[u8]) -> Result<(u32, u32, u8, Vec<u8>), &'static str> {
    if png.get(..8) != Some(&SIGNATURE[..]) {
        return Err("not a PNG");
    }
    let mut p = 8;
    let (mut w, mut h, mut channels) = (0u32, 0u32, 0u8);
    let mut z = Vec::new();
    loop {
        let len = u32::from_be_bytes(png.get(p..p + 4).ok_or("truncated")?.try_into().unwrap()) as usize;
        let kind: [u8; 4] = png.get(p + 4..p + 8).ok_or("truncated")?.try_into().unwrap();
        let data = png.get(p + 8..p + 8 + len).ok_or("truncated")?;
        let crc = u32::from_be_bytes(png.get(p + 8 + len..p + 12 + len).ok_or("truncated")?.try_into().unwrap());
        if crc != crc32(&[&kind, data]) {
            return Err("bad chunk CRC");
        }
        match &kind {
            b"IHDR" => {
                if len != 13 || data[8] != 8 || data[10] != 0 || data[11] != 0 || data[12] != 0 {
                    return Err("unsupported header");
                }
                w = u32::from_be_bytes(data[0..4].try_into().unwrap());
                h = u32::from_be_bytes(data[4..8].try_into().unwrap());
                channels = match data[9] {
                    2 => 3,
                    6 => 4,
                    _ => return Err("only RGB and RGBA"),
                };
            }
            b"IDAT" => z.extend_from_slice(data),
            b"IEND" => break,
            _ => {}
        }
        p += 12 + len;
    }
    if channels == 0 {
        return Err("no IHDR");
    }
    Ok((w, h, channels, z))
}

/// Decodes any 8-bit RGB or RGBA PNG that isn't interlaced (every deflate
/// block type, every row filter): `xtask` reading the playbook's renders
/// (feature `decode`; the image never carries it, FR-025).
#[cfg(feature = "decode")]
pub fn decode(png: &[u8]) -> Result<Decoded, &'static str> {
    let (w, h, channels, z) = chunks(png)?;
    let raw = crate::inflate::zlib(&z)?;
    let bpp = channels as usize;
    let row = w as usize * bpp;
    if raw.len() != (row + 1) * h as usize {
        return Err("wrong amount of pixel data");
    }
    let mut pixels: Vec<u8> = alloc::vec![0; row * h as usize];
    for y in 0..h as usize {
        let line = &raw[y * (row + 1)..(y + 1) * (row + 1)];
        let (filter, src) = (line[0], &line[1..]);
        for i in 0..row {
            let a = if i >= bpp { pixels[y * row + i - bpp] } else { 0 };
            let b = if y > 0 { pixels[(y - 1) * row + i] } else { 0 };
            let c = if y > 0 && i >= bpp { pixels[(y - 1) * row + i - bpp] } else { 0 };
            let x = src[i];
            pixels[y * row + i] = match filter {
                0 => x,
                1 => x.wrapping_add(a),
                2 => x.wrapping_add(b),
                3 => x.wrapping_add(((a as u16 + b as u16) / 2) as u8),
                4 => x.wrapping_add(paeth(a, b, c)),
                _ => return Err("an unknown row filter"),
            };
        }
    }
    Ok(Decoded { w, h, channels, pixels })
}

#[cfg(feature = "decode")]
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Decodes a PNG whose deflate stream uses only stored blocks and whose rows
/// use filter 0, as [`encode`] writes. Checks every CRC and the Adler-32.
pub fn decode_stored(png: &[u8]) -> Result<Decoded, &'static str> {
    let (w, h, channels, z) = chunks(png)?;
    if z.len() < 6 || z[0] & 0x0F != 8 {
        return Err("bad zlib stream");
    }
    let mut raw = Vec::new();
    let mut q = 2;
    loop {
        let head = *z.get(q).ok_or("truncated deflate")?;
        if head & 0b110 != 0 {
            return Err("only stored deflate blocks");
        }
        let len = u16::from_le_bytes([*z.get(q + 1).ok_or("truncated")?, *z.get(q + 2).ok_or("truncated")?]) as usize;
        let nlen = u16::from_le_bytes([*z.get(q + 3).ok_or("truncated")?, *z.get(q + 4).ok_or("truncated")?]);
        if nlen != !(len as u16) {
            return Err("bad stored block length");
        }
        raw.extend_from_slice(z.get(q + 5..q + 5 + len).ok_or("truncated block")?);
        q += 5 + len;
        if head & 1 == 1 {
            break;
        }
    }
    let adler = u32::from_be_bytes(z.get(q..q + 4).ok_or("no Adler-32")?.try_into().unwrap());
    if adler != adler32(&raw) {
        return Err("bad Adler-32");
    }
    let row = w as usize * channels as usize;
    if raw.len() != (row + 1) * h as usize {
        return Err("wrong amount of pixel data");
    }
    let mut pixels = Vec::with_capacity(row * h as usize);
    for r in raw.chunks(row + 1) {
        if r[0] != 0 {
            return Err("only filter 0");
        }
        pixels.extend_from_slice(&r[1..]);
    }
    Ok(Decoded { w, h, channels, pixels })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_check_value() {
        assert_eq!(crc32(&[b"123456789"]), 0xCBF4_3926);
    }

    #[test]
    fn adler32_known_value() {
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn round_trips_across_block_boundaries() {
        // 300 × 300 RGB: 270,300 bytes of rows, so several 65,535-byte blocks.
        let (w, h) = (300u32, 300u32);
        let px: Vec<u8> = (0..w * h * 3).map(|i| (i * 31 % 251) as u8).collect();
        let png = encode(w, h, 3, &px);
        let d = decode_stored(&png).unwrap();
        assert_eq!((d.w, d.h, d.channels), (w, h, 3));
        assert_eq!(d.pixels, px);
    }

    #[test]
    fn a_damaged_png_is_refused() {
        let png = encode(4, 4, 4, &[7u8; 64]);
        for i in [8, 20, 40, png.len() - 6] {
            let mut bad = png.clone();
            bad[i] ^= 1;
            assert!(decode_stored(&bad).is_err(), "byte {i}");
        }
    }
}
