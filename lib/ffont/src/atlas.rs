// SPDX-License-Identifier: Apache-2.0
//! FATL, Freally's glyph atlas: glyphs pre-rendered at fixed sizes ("strikes"),
//! so a screen with no rasterizer (the bootloader, the panic screen) can still
//! draw Noto by blending coverage. Reading and drawing allocate nothing.
//!
//! Format 1, little-endian:
//! - header (8 bytes): `FATL`, version `u16` = 1, strike count `u16`;
//! - per strike (16 bytes): px `u16`, ascent `i16`, descent `i16`, line height `i16`,
//!   glyph count `u16`, zero `u16`, glyph records' offset `u32`;
//! - per glyph (20 bytes, sorted by code point): code point `u32`, advance `i32`
//!   (1/64 px), left `i16`, top `i16`, width `u16`, height `u16`, coverage offset `u32`;
//! - the coverage bytes (`width × height` each, row by row).

pub const MAGIC: [u8; 4] = *b"FATL";
pub const VERSION: u16 = 1;
const STRIKE_LEN: usize = 16;
const GLYPH_LEN: usize = 20;

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*d.get(o)?, *d.get(o + 1)?]))
}

fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*d.get(o)?, *d.get(o + 1)?, *d.get(o + 2)?, *d.get(o + 3)?]))
}

/// A parsed atlas, borrowing its bytes.
#[derive(Clone, Copy, Debug)]
pub struct Atlas<'a> {
    data: &'a [u8],
    strikes: u16,
}

/// One size of one face.
#[derive(Clone, Copy, Debug)]
pub struct Strike<'a> {
    data: &'a [u8],
    pub px: u16,
    pub ascent: i16,
    pub descent: i16,
    pub line_height: i16,
    count: u16,
    glyphs: usize,
}

/// One glyph's placement and coverage.
#[derive(Clone, Copy, Debug)]
pub struct Glyph<'a> {
    pub advance_q6: i32,
    pub left: i16,
    pub top: i16,
    pub w: u16,
    pub h: u16,
    pub alpha: &'a [u8],
}

impl<'a> Atlas<'a> {
    pub fn parse(data: &'a [u8]) -> Option<Atlas<'a>> {
        if data.get(0..4)? != MAGIC || u16_at(data, 4)? != VERSION {
            return None;
        }
        let strikes = u16_at(data, 6)?;
        data.get(8..8 + STRIKE_LEN * strikes as usize)?;
        Some(Atlas { data, strikes })
    }

    pub fn strike(&self, i: usize) -> Option<Strike<'a>> {
        if i >= self.strikes as usize {
            return None;
        }
        let o = 8 + STRIKE_LEN * i;
        let d = self.data;
        let count = u16_at(d, o + 8)?;
        let glyphs = u32_at(d, o + 12)? as usize;
        d.get(glyphs..glyphs + GLYPH_LEN * count as usize)?;
        Some(Strike {
            data: d,
            px: u16_at(d, o)?,
            ascent: u16_at(d, o + 2)? as i16,
            descent: u16_at(d, o + 4)? as i16,
            line_height: u16_at(d, o + 6)? as i16,
            count,
            glyphs,
        })
    }
}

impl<'a> Strike<'a> {
    fn record(&self, i: usize) -> Option<Glyph<'a>> {
        let d = self.data;
        let o = self.glyphs + GLYPH_LEN * i;
        let w = u16_at(d, o + 12)?;
        let h = u16_at(d, o + 14)?;
        let at = u32_at(d, o + 16)? as usize;
        Some(Glyph {
            advance_q6: u32_at(d, o + 4)? as i32,
            left: u16_at(d, o + 8)? as i16,
            top: u16_at(d, o + 10)? as i16,
            w,
            h,
            alpha: d.get(at..at + w as usize * h as usize)?,
        })
    }

    /// The glyph for `c`, or `?` when the atlas doesn't have it.
    pub fn glyph(&self, c: char) -> Option<Glyph<'a>> {
        let find = |c: u32| {
            let (mut lo, mut hi) = (0usize, self.count as usize);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let cp = u32_at(self.data, self.glyphs + GLYPH_LEN * mid)?;
                if cp == c {
                    return self.record(mid);
                }
                if cp < c { lo = mid + 1 } else { hi = mid }
            }
            None
        };
        find(c as u32).or_else(|| find('?' as u32))
    }

    /// The width of `text` in pixels, as [`draw_text`] places it.
    pub fn measure(&self, text: &str) -> i32 {
        self.measure_parts(&[text])
    }

    /// The width of several strings drawn as one run.
    pub fn measure_parts(&self, parts: &[&str]) -> i32 {
        let pen: i32 = parts.iter().flat_map(|p| p.chars()).filter_map(|c| self.glyph(c)).map(|g| g.advance_q6).sum();
        (pen + 32) >> 6
    }
}

/// Anything text can be blended into: `0x00RRGGBB` pixels.
pub trait Target {
    fn size(&self) -> (usize, usize);
    fn get(&self, x: usize, y: usize) -> u32;
    fn set(&mut self, x: usize, y: usize, rgb: u32);
}

/// `src` over `dst` at coverage `a` (0–255), per channel, rounded.
pub fn blend(dst: u32, src: u32, a: u8) -> u32 {
    let a = a as u32;
    let mut out = 0;
    for shift in [0, 8, 16] {
        let d = (dst >> shift) & 0xFF;
        let s = (src >> shift) & 0xFF;
        out |= ((s * a + d * (255 - a) + 127) / 255) << shift;
    }
    out
}

/// Draws `text` with its pen starting at `x` on the baseline `y`; returns the
/// pen's end, in pixels. Glyphs are placed at whole pixels.
pub fn draw_text(t: &mut impl Target, s: &Strike, x: i32, y: i32, text: &str, rgb: u32) -> i32 {
    draw_parts(t, s, x, y, &[text], rgb)
}

/// Draws several strings as one run (one pen, so spacing is as if joined).
pub fn draw_parts(t: &mut impl Target, s: &Strike, x: i32, y: i32, parts: &[&str], rgb: u32) -> i32 {
    let (w, h) = t.size();
    let mut pen = x << 6;
    for c in parts.iter().flat_map(|p| p.chars()) {
        let Some(g) = s.glyph(c) else { continue };
        let gx = ((pen + 32) >> 6) + g.left as i32;
        let gy = y - g.top as i32;
        for row in 0..g.h as i32 {
            let py = gy + row;
            if py < 0 || py >= h as i32 {
                continue;
            }
            for col in 0..g.w as i32 {
                let px = gx + col;
                if px < 0 || px >= w as i32 {
                    continue;
                }
                let a = g.alpha[(row * g.w as i32 + col) as usize];
                if a == 0 {
                    continue;
                }
                let dst = t.get(px as usize, py as usize);
                t.set(px as usize, py as usize, blend(dst, rgb, a));
            }
        }
        pen += g.advance_q6;
    }
    (pen + 32) >> 6
}

/// Builds an atlas from `(font, px, characters)` strikes.
#[cfg(feature = "raster")]
pub fn build(strikes: &[(&crate::ttf::Font, f32, &str)]) -> alloc::vec::Vec<u8> {
    use alloc::vec::Vec;
    let mut glyph_sets: Vec<Vec<(u32, crate::ttf::GlyphBitmap)>> = Vec::new();
    for (font, px, chars) in strikes {
        let mut set: Vec<(u32, crate::ttf::GlyphBitmap)> = Vec::new();
        for c in chars.chars() {
            if set.iter().any(|(cp, _)| *cp == c as u32) {
                continue;
            }
            set.push((c as u32, crate::ttf::render(font, font.glyph_index(c), *px)));
        }
        set.sort_by_key(|(cp, _)| *cp);
        glyph_sets.push(set);
    }
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(strikes.len() as u16).to_le_bytes());
    let mut records_at = 8 + STRIKE_LEN * strikes.len();
    let total_records: usize = glyph_sets.iter().map(|s| s.len()).sum();
    let mut alpha_at = records_at + GLYPH_LEN * total_records;
    let mut records = Vec::new();
    let mut alpha = Vec::new();
    for ((font, px, _), set) in strikes.iter().zip(&glyph_sets) {
        let s = px / font.units_per_em as f32;
        let ascent = crate::fmath::round(font.ascender as f32 * s) as i16;
        let descent = crate::fmath::round(-(font.descender as f32) * s) as i16;
        let line =
            crate::fmath::round((font.ascender as f32 - font.descender as f32 + font.line_gap as f32) * s) as i16;
        out.extend_from_slice(&(*px as u16).to_le_bytes());
        out.extend_from_slice(&ascent.to_le_bytes());
        out.extend_from_slice(&descent.to_le_bytes());
        out.extend_from_slice(&line.to_le_bytes());
        out.extend_from_slice(&(set.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(records_at as u32).to_le_bytes());
        records_at += GLYPH_LEN * set.len();
        for (cp, g) in set {
            records.extend_from_slice(&cp.to_le_bytes());
            records.extend_from_slice(&g.advance_q6.to_le_bytes());
            records.extend_from_slice(&g.left.to_le_bytes());
            records.extend_from_slice(&g.top.to_le_bytes());
            records.extend_from_slice(&g.w.to_le_bytes());
            records.extend_from_slice(&g.h.to_le_bytes());
            records.extend_from_slice(&(alpha_at as u32).to_le_bytes());
            alpha_at += g.alpha.len();
            alpha.extend_from_slice(&g.alpha);
        }
    }
    out.extend_from_slice(&records);
    out.extend_from_slice(&alpha);
    out
}
