// SPDX-License-Identifier: Apache-2.0
//! FIMG (PRD § 3): Freally's own pixel format, drawn with no decoding.
//!
//! A 32-byte header, then `width × height` pixels, BGRA, 8 bits each,
//! premultiplied by alpha, rows tightly packed. `scale` says which UI scale
//! it's for, in quarters of 100 % (4 = 100 %, 6 = 150 %, 12 = 300 %: version
//! 2, TASK-090A, every scale the OS offers); `slice` holds the 9-slice insets
//! (left, top, right, bottom; 0 = none).

use alloc::vec::Vec;

pub const MAGIC: [u8; 4] = *b"FIMG";
/// 2: `scale` in quarters of 100 % (version 1 held 1, 2 or 3).
pub const VERSION: u16 = 2;
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
    if !(4..=12).contains(&scale) {
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

/// A render's PNG (straight RGBA, drawn at `render_scale`x) as a FIMG for the
/// UI scale `pct` (100–300 %, a multiple of 25): area-averaged down in
/// linear light with alpha premultiplied ([`downscale_linear`]), its 9-slice
/// insets (given at the render's scale) scaled with it. For the host: the
/// image carries the FIMGs, never a PNG decoder.
#[cfg(feature = "decode")]
pub fn from_png(png: &[u8], pct: u32, render_scale: u32, slice: [u32; 4]) -> Result<Vec<u8>, &'static str> {
    if !(100..=300).contains(&pct) || pct % 25 != 0 || pct > render_scale * 100 {
        return Err("a scale the OS doesn't offer, or above the render's");
    }
    let d = crate::png::decode(png)?;
    if d.channels != 4 {
        return Err("a render that isn't RGBA");
    }
    let (num, den) = reduced(pct, render_scale * 100);
    // At the render's own scale the pixels go in as they are.
    let (w, h, px) =
        if num == den { (d.w, d.h, from_rgba(&d.pixels)) } else { downscale_linear(d.w, d.h, &d.pixels, num, den) };
    let inset = |v: u32| ((v * num + den / 2) / den) as u16;
    Ok(encode(w, h, (pct / 25) as u8, slice.map(inset), &px))
}

/// `a/b` in lowest terms.
#[cfg(feature = "decode")]
fn reduced(a: u32, b: u32) -> (u32, u32) {
    let (mut x, mut y) = (a, b);
    while y != 0 {
        (x, y) = (y, x % y);
    }
    (a / x.max(1), b / x.max(1))
}

/// sRGB (0–255) to linear light (0–1).
#[cfg(feature = "decode")]
fn linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// Linear light (0–1) to sRGB (0–255).
#[cfg(feature = "decode")]
fn srgb(l: f32) -> u8 {
    let l = l.clamp(0.0, 1.0);
    let c = if l <= 0.0031308 { l * 12.92 } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 };
    (c * 255.0 + 0.5) as u8
}

/// Straight RGBA `w × h` brought down by `num/den` the way a camera's sensor
/// would see it: each target pixel the coverage-weighted mean of the source
/// pixels under it, in linear light with alpha premultiplied, so a thin
/// highlight keeps its brightness and an edge neither darkens nor fringes.
/// The result is premultiplied BGRA (sRGB), as FIMG holds it.
#[cfg(feature = "decode")]
pub fn downscale_linear(w: u32, h: u32, rgba: &[u8], num: u32, den: u32) -> (u32, u32, Vec<u8>) {
    let (w2, h2) = ((w * num).div_ceil(den).max(1), (h * num).div_ceil(den).max(1));
    // Premultiplied linear light, once.
    let lin: Vec<[f32; 4]> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| {
            let a = p[3] as f32 / 255.0;
            [linear(p[0]) * a, linear(p[1]) * a, linear(p[2]) * a, a]
        })
        .collect();
    let span = |i: u32| (i * den, (i + 1) * den);
    let mut out = Vec::with_capacity((w2 * h2 * 4) as usize);
    for ty in 0..h2 {
        let (y0, y1) = span(ty);
        for tx in 0..w2 {
            let (x0, x1) = span(tx);
            let mut acc = [0f64; 4];
            let mut weight = 0f64;
            for sy in y0 / num..y1.div_ceil(num).min(h) {
                let wy = (y1.min((sy + 1) * num) - y0.max(sy * num)) as f64;
                for sx in x0 / num..x1.div_ceil(num).min(w) {
                    let wx = (x1.min((sx + 1) * num) - x0.max(sx * num)) as f64;
                    let p = lin[(sy * w + sx) as usize];
                    for c in 0..4 {
                        acc[c] += p[c] as f64 * wx * wy;
                    }
                    weight += wx * wy;
                }
            }
            let a = (acc[3] / weight.max(1e-12)) as f32;
            if a <= 0.0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            // Back to straight sRGB, then premultiplied as FIMG keeps it.
            let a8 = (a * 255.0 + 0.5) as u32;
            let ch = |c: usize| {
                let straight = srgb((acc[c] / weight) as f32 / a);
                ((straight as u32 * a8 + 127) / 255) as u8
            };
            out.extend_from_slice(&[ch(2), ch(1), ch(0), a8.min(255) as u8]);
        }
    }
    (w2, h2, out)
}
