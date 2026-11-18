// SPDX-License-Identifier: Apache-2.0
//! A TrueType parser, written from the OpenType specification: the table
//! directory, `head`, `hhea`, `maxp`, `hmtx`, `cmap` (formats 4 and 12), `loca`
//! and `glyf` (simple and composite glyphs). Every read is bounds-checked: a
//! damaged font gives an error or an empty glyph, never a crash.

use crate::raster::Raster;
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FontError {
    Truncated,
    NotTrueType,
    MissingTable([u8; 4]),
    Unsupported,
}

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*d.get(o)?, *d.get(o + 1)?]))
}

fn i16_at(d: &[u8], o: usize) -> Option<i16> {
    u16_at(d, o).map(|v| v as i16)
}

fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes([*d.get(o)?, *d.get(o + 1)?, *d.get(o + 2)?, *d.get(o + 3)?]))
}

/// A parsed TrueType font, borrowing its bytes.
#[derive(Clone, Copy, Debug)]
pub struct Font<'a> {
    data: &'a [u8],
    pub units_per_em: u16,
    pub ascender: i16,
    pub descender: i16,
    pub line_gap: i16,
    long_loca: bool,
    num_glyphs: u16,
    num_hmetrics: u16,
    loca: &'a [u8],
    glyf: &'a [u8],
    hmtx: &'a [u8],
    cmap: &'a [u8],
    cmap_format: u16,
}

/// One point of an outline, in font units, y up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
    pub on_curve: bool,
}

impl<'a> Font<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Font<'a>, FontError> {
        let version = u32_at(data, 0).ok_or(FontError::Truncated)?;
        if version != 0x0001_0000 && version != u32::from_be_bytes(*b"true") {
            return Err(FontError::NotTrueType);
        }
        let n = u16_at(data, 4).ok_or(FontError::Truncated)? as usize;
        let table = |tag: &[u8; 4]| -> Result<&'a [u8], FontError> {
            for i in 0..n {
                let rec = 12 + 16 * i;
                if data.get(rec..rec + 4) == Some(&tag[..]) {
                    let off = u32_at(data, rec + 8).ok_or(FontError::Truncated)? as usize;
                    let len = u32_at(data, rec + 12).ok_or(FontError::Truncated)? as usize;
                    return data
                        .get(off..off.checked_add(len).ok_or(FontError::Truncated)?)
                        .ok_or(FontError::Truncated);
                }
            }
            Err(FontError::MissingTable(*tag))
        };
        let head = table(b"head")?;
        let hhea = table(b"hhea")?;
        let maxp = table(b"maxp")?;
        let cmap_table = table(b"cmap")?;
        let t = FontError::Truncated;

        // cmap: prefer a full-Unicode format 12 subtable, else a BMP format 4 one.
        let count = u16_at(cmap_table, 2).ok_or(t)? as usize;
        let mut best: Option<(u8, &'a [u8], u16)> = None;
        for i in 0..count {
            let rec = 4 + 8 * i;
            let platform = u16_at(cmap_table, rec).ok_or(t)?;
            let encoding = u16_at(cmap_table, rec + 2).ok_or(t)?;
            let off = u32_at(cmap_table, rec + 4).ok_or(t)? as usize;
            let sub = cmap_table.get(off..).ok_or(t)?;
            let format = u16_at(sub, 0).ok_or(t)?;
            let rank = match (platform, encoding, format) {
                (3, 10, 12) | (0, 4, 12) | (0, 6, 12) => 3,
                (3, 1, 4) | (0, 3, 4) | (0, 1, 4) | (0, 0, 4) => 2,
                _ => 0,
            };
            if rank > 0 && best.is_none_or(|b| rank > b.0) {
                best = Some((rank, sub, format));
            }
        }
        let (_, cmap, cmap_format) = best.ok_or(FontError::Unsupported)?;

        Ok(Font {
            data,
            units_per_em: u16_at(head, 18).ok_or(t)?,
            long_loca: i16_at(head, 50).ok_or(t)? == 1,
            ascender: i16_at(hhea, 4).ok_or(t)?,
            descender: i16_at(hhea, 6).ok_or(t)?,
            line_gap: i16_at(hhea, 8).ok_or(t)?,
            num_hmetrics: u16_at(hhea, 34).ok_or(t)?,
            num_glyphs: u16_at(maxp, 4).ok_or(t)?,
            loca: table(b"loca")?,
            glyf: table(b"glyf")?,
            hmtx: table(b"hmtx")?,
            cmap,
            cmap_format,
        })
    }

    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// The glyph for `c` (0, the "missing" glyph, when the font has none).
    pub fn glyph_index(&self, c: char) -> u16 {
        let c = c as u32;
        let d = self.cmap;
        let found = if self.cmap_format == 12 {
            (|| {
                let groups = u32_at(d, 12)? as usize;
                let (mut lo, mut hi) = (0usize, groups);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    let g = 16 + 12 * mid;
                    let start = u32_at(d, g)?;
                    let end = u32_at(d, g + 4)?;
                    if c < start {
                        hi = mid;
                    } else if c > end {
                        lo = mid + 1;
                    } else {
                        return Some((u32_at(d, g + 8)? + (c - start)) as u16);
                    }
                }
                None
            })()
        } else {
            (|| {
                if c > 0xFFFF {
                    return None;
                }
                let seg_x2 = u16_at(d, 6)? as usize;
                let ends = 14;
                let starts = ends + seg_x2 + 2;
                let deltas = starts + seg_x2;
                let ranges = deltas + seg_x2;
                for i in 0..seg_x2 / 2 {
                    let end = u16_at(d, ends + 2 * i)? as u32;
                    if end < c {
                        continue;
                    }
                    let start = u16_at(d, starts + 2 * i)? as u32;
                    if start > c {
                        return None;
                    }
                    let delta = u16_at(d, deltas + 2 * i)?;
                    let range = u16_at(d, ranges + 2 * i)? as usize;
                    if range == 0 {
                        return Some((c as u16).wrapping_add(delta));
                    }
                    let at = ranges + 2 * i + range + 2 * (c - start) as usize;
                    let g = u16_at(d, at)?;
                    return Some(if g == 0 { 0 } else { g.wrapping_add(delta) });
                }
                None
            })()
        };
        found.filter(|&g| g < self.num_glyphs).unwrap_or(0)
    }

    /// The advance width of glyph `g`, in font units.
    pub fn advance(&self, g: u16) -> u16 {
        let i = (g.min(self.num_hmetrics.saturating_sub(1))) as usize;
        u16_at(self.hmtx, 4 * i).unwrap_or(0)
    }

    fn glyph_data(&self, g: u16) -> Option<&'a [u8]> {
        if g >= self.num_glyphs {
            return None;
        }
        let g = g as usize;
        let (a, b) = if self.long_loca {
            (u32_at(self.loca, 4 * g)? as usize, u32_at(self.loca, 4 * g + 4)? as usize)
        } else {
            (2 * u16_at(self.loca, 2 * g)? as usize, 2 * u16_at(self.loca, 2 * g + 2)? as usize)
        };
        if b <= a {
            return Some(&[]);
        }
        self.glyf.get(a..b)
    }

    /// The glyph's bounding box `(x_min, y_min, x_max, y_max)` in font units, if it has an outline.
    pub fn bbox(&self, g: u16) -> Option<(i16, i16, i16, i16)> {
        let d = self.glyph_data(g)?;
        if d.len() < 10 {
            return None;
        }
        Some((i16_at(d, 2)?, i16_at(d, 4)?, i16_at(d, 6)?, i16_at(d, 8)?))
    }

    /// The glyph's contours (each a closed loop of points), composites resolved.
    pub fn outline(&self, g: u16, contours: &mut Vec<Vec<Point>>) {
        self.outline_into(g, contours, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], 0);
    }

    fn outline_into(&self, g: u16, out: &mut Vec<Vec<Point>>, m: [f32; 6], depth: u8) {
        if depth > 8 {
            return; // a composite loop in a damaged font
        }
        let Some(d) = self.glyph_data(g) else { return };
        let Some(n_contours) = i16_at(d, 0) else { return };
        if n_contours >= 0 {
            let _ = self.simple(d, n_contours as usize, out, m);
        } else {
            let _ = self.composite(d, out, m, depth);
        }
    }

    fn simple(&self, d: &[u8], n: usize, out: &mut Vec<Vec<Point>>, m: [f32; 6]) -> Option<()> {
        let mut ends = Vec::with_capacity(n);
        for i in 0..n {
            ends.push(u16_at(d, 10 + 2 * i)? as usize);
        }
        let n_points = ends.last().map_or(0, |&e| e + 1);
        let ins_len = u16_at(d, 10 + 2 * n)? as usize;
        let mut p = 12 + 2 * n + ins_len;
        let mut flags = Vec::with_capacity(n_points);
        while flags.len() < n_points {
            let f = *d.get(p)?;
            p += 1;
            flags.push(f);
            if f & 8 != 0 {
                let r = *d.get(p)?;
                p += 1;
                for _ in 0..r {
                    flags.push(f);
                }
            }
        }
        flags.truncate(n_points);
        let mut xs = Vec::with_capacity(n_points);
        let mut v: i32 = 0;
        for &f in &flags {
            if f & 2 != 0 {
                let dx = *d.get(p)? as i32;
                p += 1;
                v += if f & 16 != 0 { dx } else { -dx };
            } else if f & 16 == 0 {
                v += i16_at(d, p)? as i32;
                p += 2;
            }
            xs.push(v);
        }
        let mut v: i32 = 0;
        let mut start = 0;
        let mut contour = Vec::new();
        for (i, &f) in flags.iter().enumerate() {
            if f & 4 != 0 {
                let dy = *d.get(p)? as i32;
                p += 1;
                v += if f & 32 != 0 { dy } else { -dy };
            } else if f & 32 == 0 {
                v += i16_at(d, p)? as i32;
                p += 2;
            }
            let (x, y) = (xs[i] as f32, v as f32);
            contour.push(Point { x: m[0] * x + m[2] * y + m[4], y: m[1] * x + m[3] * y + m[5], on_curve: f & 1 != 0 });
            if ends.get(start) == Some(&i) {
                out.push(core::mem::take(&mut contour));
                start += 1;
            }
        }
        Some(())
    }

    fn composite(&self, d: &[u8], out: &mut Vec<Vec<Point>>, m: [f32; 6], depth: u8) -> Option<()> {
        let mut p = 10;
        loop {
            let flags = u16_at(d, p)?;
            let glyph = u16_at(d, p + 2)?;
            p += 4;
            let (a1, a2) = if flags & 1 != 0 {
                let r = (i16_at(d, p)? as f32, i16_at(d, p + 2)? as f32);
                p += 4;
                r
            } else {
                let r = (*d.get(p)? as i8 as f32, *d.get(p + 1)? as i8 as f32);
                p += 2;
                r
            };
            let f2 = |o: usize| i16_at(d, o).map(|v| v as f32 / 16384.0);
            let (mut sa, mut sb, mut sc, mut sd) = (1.0, 0.0, 0.0, 1.0);
            if flags & 8 != 0 {
                sa = f2(p)?;
                sd = sa;
                p += 2;
            } else if flags & 0x40 != 0 {
                sa = f2(p)?;
                sd = f2(p + 2)?;
                p += 4;
            } else if flags & 0x80 != 0 {
                sa = f2(p)?;
                sb = f2(p + 2)?;
                sc = f2(p + 4)?;
                sd = f2(p + 6)?;
                p += 8;
            }
            // Offsets as x/y values (point matching, flag 2 clear, is treated as no offset).
            let (dx, dy) = if flags & 2 != 0 { (a1, a2) } else { (0.0, 0.0) };
            // child → this glyph: [sa sc; sb sd]·p + (dx, dy); then this glyph → caller: m.
            let child = [
                m[0] * sa + m[2] * sb,
                m[1] * sa + m[3] * sb,
                m[0] * sc + m[2] * sd,
                m[1] * sc + m[3] * sd,
                m[0] * dx + m[2] * dy + m[4],
                m[1] * dx + m[3] * dy + m[5],
            ];
            self.outline_into(glyph, out, child, depth + 1);
            if flags & 0x20 == 0 {
                return Some(());
            }
        }
    }
}

/// A rendered glyph: `w × h` coverage, placed `left` px right of the pen and
/// `top` px above the baseline; the pen then moves `advance_q6` 1/64 px.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlyphBitmap {
    pub w: u16,
    pub h: u16,
    pub left: i16,
    pub top: i16,
    pub advance_q6: i32,
    pub alpha: Vec<u8>,
}

/// Renders glyph `g` at `px` pixels per em.
pub fn render(font: &Font, g: u16, px: f32) -> GlyphBitmap {
    let s = px / font.units_per_em as f32;
    let advance_q6 = crate::fmath::round(font.advance(g) as f32 * s * 64.0) as i32;
    let Some((x_min, y_min, x_max, y_max)) = font.bbox(g) else {
        return GlyphBitmap { w: 0, h: 0, left: 0, top: 0, advance_q6, alpha: Vec::new() };
    };
    let left = crate::fmath::floor(x_min as f32 * s) as i32;
    let right = crate::fmath::ceil(x_max as f32 * s) as i32;
    let top = crate::fmath::ceil(y_max as f32 * s) as i32;
    let bottom = crate::fmath::floor(y_min as f32 * s) as i32;
    let w = (right - left).max(0) as usize + 1;
    let h = (top - bottom).max(0) as usize + 1;
    let mut contours = Vec::new();
    font.outline(g, &mut contours);
    let mut r = Raster::new(w, h);
    let tx = |p: &Point| (p.x * s - left as f32, top as f32 - p.y * s);
    for c in &contours {
        if c.len() < 2 {
            continue;
        }
        // Start at an on-curve point, or at the midpoint of the first two off-curve ones.
        let n = c.len();
        let first_on = c.iter().position(|p| p.on_curve);
        let (start, start_pt) = match first_on {
            Some(i) => (i, tx(&c[i])),
            None => {
                let (a, b) = (tx(&c[0]), tx(&c[1]));
                (0, ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0))
            }
        };
        let mut cur = start_pt;
        let mut ctrl: Option<(f32, f32)> = None;
        for k in 1..=n {
            let p = &c[(start + k) % n];
            let q = tx(p);
            match (p.on_curve, ctrl) {
                (true, None) => {
                    r.line(cur.0, cur.1, q.0, q.1);
                    cur = q;
                }
                (true, Some(cp)) => {
                    r.quad(cur, cp, q);
                    cur = q;
                    ctrl = None;
                }
                (false, None) => ctrl = Some(q),
                (false, Some(cp)) => {
                    let mid = ((cp.0 + q.0) / 2.0, (cp.1 + q.1) / 2.0);
                    r.quad(cur, cp, mid);
                    cur = mid;
                    ctrl = Some(q);
                }
            }
        }
        // Close the contour back to its start.
        match ctrl {
            Some(cp) => r.quad(cur, cp, start_pt),
            None => r.line(cur.0, cur.1, start_pt.0, start_pt.1),
        }
    }
    GlyphBitmap { w: w as u16, h: h as u16, left: left as i16, top: top as i16, advance_q6, alpha: r.finish() }
}
