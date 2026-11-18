// SPDX-License-Identifier: Apache-2.0
//! FIMG (PRD § 3): Freally's own pixel format, drawn with no decoding.
//!
//! A 32-byte header, then `width × height` pixels, BGRA, 8 bits each,
//! premultiplied by alpha, rows tightly packed. `scale` says which UI scale
//! it's for (1, 2 or 3); `slice` holds the 9-slice insets (left, top, right,
//! bottom; 0 = none).

use alloc::vec::Vec;

pub const MAGIC: [u8; 4] = *b"FIMG";
pub const VERSION: u16 = 1;
/// BGRA, 8 bits a channel, premultiplied.
pub const BGRA8_PREMULTIPLIED: u16 = 1;
pub const HEADER_LEN: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fimg<'a> {
    pub width: u32,
    pub height: u32,
    pub scale: u8,
    pub slice: [u16; 4],
    /// BGRA premultiplied, `width * height * 4` bytes.
    pub pixels: &'a [u8],
}

impl Fimg<'_> {
    /// The pixel at (x, y) as (b, g, r, a), premultiplied.
    pub fn at(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [self.pixels[i], self.pixels[i + 1], self.pixels[i + 2], self.pixels[i + 3]]
    }
}

pub fn encode(width: u32, height: u32, scale: u8, slice: [u16; 4], pixels: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + pixels.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&BGRA8_PREMULTIPLIED.to_le_bytes());
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.push(scale);
    for s in slice {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out.extend_from_slice(&[0; 7]);
    out.extend_from_slice(pixels);
    out
}

pub fn parse(bytes: &[u8]) -> Result<Fimg<'_>, &'static str> {
    let h = bytes.get(..HEADER_LEN).ok_or("a FIMG shorter than its header")?;
    if h[..4] != MAGIC {
        return Err("not a FIMG");
    }
    let u16_at = |at: usize| u16::from_le_bytes([h[at], h[at + 1]]);
    let u32_at = |at: usize| u32::from_le_bytes([h[at], h[at + 1], h[at + 2], h[at + 3]]);
    if u16_at(4) != VERSION || u16_at(6) != BGRA8_PREMULTIPLIED {
        return Err("a FIMG of another version or format");
    }
    let (width, height, scale) = (u32_at(8), u32_at(12), h[16]);
    if !(1..=3).contains(&scale) {
        return Err("a FIMG for no known scale");
    }
    let slice = [u16_at(17), u16_at(19), u16_at(21), u16_at(23)];
    let n = (width as usize).checked_mul(height as usize).and_then(|p| p.checked_mul(4)).ok_or("a FIMG too big")?;
    let pixels = bytes.get(HEADER_LEN..HEADER_LEN + n).ok_or("a FIMG shorter than its pixels")?;
    if bytes.len() != HEADER_LEN + n {
        return Err("a FIMG longer than its pixels");
    }
    if slice[0] as u32 + slice[2] as u32 > width || slice[1] as u32 + slice[3] as u32 > height {
        return Err("9-slice insets wider than the image");
    }
    Ok(Fimg { width, height, scale, slice, pixels })
}

/// Straight RGBA (a PNG's) to premultiplied BGRA.
pub fn from_rgba(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    for p in rgba.as_chunks::<4>().0 {
        let a = p[3] as u32;
        let pm = |c: u8| ((c as u32 * a + 127) / 255) as u8;
        out.extend_from_slice(&[pm(p[2]), pm(p[1]), pm(p[0]), p[3]]);
    }
    out
}

/// A render's PNG (straight RGBA, drawn at `render_scale`) as a FIMG for UI
/// scale `scale`: premultiplied, area-averaged down, its 9-slice insets
/// (given at the render's scale) scaled with it. For the host: the image
/// carries the FIMGs, never a PNG decoder.
#[cfg(feature = "decode")]
pub fn from_png(png: &[u8], scale: u32, render_scale: u32, slice: [u32; 4]) -> Result<Vec<u8>, &'static str> {
    let d = crate::png::decode(png)?;
    if d.channels != 4 {
        return Err("a render that isn't RGBA");
    }
    let (w, h, px) = downscale(d.w, d.h, &from_rgba(&d.pixels), scale, render_scale);
    Ok(encode(w, h, scale as u8, slice.map(|v| (v * scale / render_scale) as u16), &px))
}

/// Area-averaged downscale of premultiplied pixels by `num/den` (2/3 for 3x
/// to 2x, 1/3 for 3x to 1x): each target pixel is the coverage-weighted mean
/// of the source pixels under it. Premultiplied, so edges don't darken.
pub fn downscale(w: u32, h: u32, px: &[u8], num: u32, den: u32) -> (u32, u32, Vec<u8>) {
    let (w2, h2) = ((w * num).div_ceil(den).max(1), (h * num).div_ceil(den).max(1));
    // Source extent of target pixel i along an axis, in 1/num source units.
    let span = |i: u32| (i * den, (i + 1) * den);
    let mut out = Vec::with_capacity((w2 * h2 * 4) as usize);
    for ty in 0..h2 {
        let (y0, y1) = span(ty);
        for tx in 0..w2 {
            let (x0, x1) = span(tx);
            let mut acc = [0u64; 4];
            let mut weight = 0u64;
            for sy in y0 / num..y1.div_ceil(num).min(h) {
                let wy = (y1.min((sy + 1) * num) - y0.max(sy * num)) as u64;
                for sx in x0 / num..x1.div_ceil(num).min(w) {
                    let wx = (x1.min((sx + 1) * num) - x0.max(sx * num)) as u64;
                    let i = ((sy * w + sx) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += px[i + c] as u64 * wx * wy;
                    }
                    weight += wx * wy;
                }
            }
            for a in acc {
                out.push(((a + weight / 2) / weight.max(1)) as u8);
            }
        }
    }
    (w2, h2, out)
}
