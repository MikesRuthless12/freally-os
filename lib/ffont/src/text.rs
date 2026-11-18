// SPDX-License-Identifier: Apache-2.0
//! Text at run time (TASK-053): Noto parsed in user space, each glyph
//! rasterized the first time it's drawn and kept, runs laid out on sub-pixel
//! advances, and coverage blended in **linear light**. Mixed in sRGB, light
//! words on the dark ground would come out thin; mixed linearly they keep
//! their weight (dark words on the light ground come out a shade lighter, as
//! in every linear-light renderer).
//!
//! A run is drawn from a **fallback chain** of faces: each character from the
//! first face that has it (Noto Sans lacks the arrows and ✓ the coverage set
//! asks for: Noto Sans Symbols 2 and Mono have them).

use crate::atlas::Target;
use crate::ttf::{self, Font, FontError, GlyphBitmap};
use alloc::collections::BTreeMap;

/// sRGB (8 bits) to linear light (12 bits, 0..=4095): IEC 61966-2-1's curve,
/// tabulated (Freally's maths has no `powf`; the table is exact).
const TO_LINEAR: [u16; 256] = [
    0, 1, 2, 4, 5, 6, 7, 9, 10, 11, 12, 14, 15, 16, 18, 20, 21, 23, 25, 27, 29, 31, 33, 35, 37, 40, 42, 45, 48, 50, 53,
    56, 59, 62, 66, 69, 72, 76, 79, 83, 87, 91, 95, 99, 103, 107, 112, 116, 121, 126, 131, 136, 141, 146, 151, 156,
    162, 168, 173, 179, 185, 191, 197, 204, 210, 216, 223, 230, 237, 244, 251, 258, 265, 273, 280, 288, 296, 304, 312,
    320, 329, 337, 346, 354, 363, 372, 381, 390, 400, 409, 419, 428, 438, 448, 458, 469, 479, 490, 500, 511, 522, 533,
    544, 555, 567, 578, 590, 602, 614, 626, 639, 651, 664, 676, 689, 702, 715, 728, 742, 755, 769, 783, 797, 811, 825,
    840, 854, 869, 884, 899, 914, 929, 945, 960, 976, 992, 1008, 1024, 1041, 1057, 1074, 1091, 1108, 1125, 1142, 1159,
    1177, 1195, 1213, 1231, 1249, 1267, 1286, 1304, 1323, 1342, 1361, 1381, 1400, 1420, 1440, 1459, 1480, 1500, 1520,
    1541, 1562, 1582, 1603, 1625, 1646, 1668, 1689, 1711, 1733, 1755, 1778, 1800, 1823, 1846, 1869, 1892, 1916, 1939,
    1963, 1987, 2011, 2035, 2059, 2084, 2109, 2133, 2159, 2184, 2209, 2235, 2260, 2286, 2312, 2339, 2365, 2392, 2419,
    2446, 2473, 2500, 2527, 2555, 2583, 2611, 2639, 2668, 2696, 2725, 2754, 2783, 2812, 2841, 2871, 2901, 2931, 2961,
    2991, 3022, 3052, 3083, 3114, 3146, 3177, 3209, 3240, 3272, 3304, 3337, 3369, 3402, 3435, 3468, 3501, 3535, 3568,
    3602, 3636, 3670, 3705, 3739, 3774, 3809, 3844, 3879, 3915, 3950, 3986, 4022, 4059, 4095,
];

/// Linear light back to the nearest sRGB value.
fn to_srgb(lin: u32) -> u32 {
    let i = TO_LINEAR.partition_point(|&v| (v as u32) < lin);
    match i {
        0 => 0,
        256 => 255,
        i if lin - TO_LINEAR[i - 1] as u32 <= TO_LINEAR[i] as u32 - lin => i as u32 - 1,
        i => i as u32,
    }
}

/// `src` over `dst` at coverage `a` (0–255), mixed in linear light.
pub fn blend(dst: u32, src: u32, a: u8) -> u32 {
    let a = a as u32;
    let mut out = 0;
    for shift in [0, 8, 16] {
        let d = TO_LINEAR[((dst >> shift) & 0xFF) as usize] as u32;
        let s = TO_LINEAR[((src >> shift) & 0xFF) as usize] as u32;
        out |= to_srgb((s * a + d * (255 - a) + 127) / 255) << shift;
    }
    out
}

/// A font, ready to draw at any size: its glyphs rendered on first use.
pub struct Face<'a> {
    font: Font<'a>,
    /// By (glyph, size in 1/64 px).
    cache: BTreeMap<(u16, u32), GlyphBitmap>,
}

impl core::fmt::Debug for Face<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Face").field("cached", &self.cache.len()).finish()
    }
}

impl<'a> Face<'a> {
    pub fn new(data: &'a [u8]) -> Result<Face<'a>, FontError> {
        Ok(Face { font: Font::parse(data)?, cache: BTreeMap::new() })
    }

    pub fn font(&self) -> &Font<'a> {
        &self.font
    }

    /// Whether the font draws `c` (else it would draw its missing-glyph box).
    pub fn has(&self, c: char) -> bool {
        self.font.glyph_index(c) != 0
    }

    /// The glyph for `c` at `px` pixels per em.
    pub fn glyph(&mut self, c: char, px: f32) -> &GlyphBitmap {
        let g = self.font.glyph_index(c);
        let Face { font, cache } = self;
        cache.entry((g, (px * 64.0) as u32)).or_insert_with(|| ttf::render(font, g, px))
    }

    /// Ascent and descent (both positive) and the line's height, in whole
    /// pixels at `px`.
    pub fn metrics(&self, px: f32) -> (i32, i32, i32) {
        let s = px / self.font.units_per_em as f32;
        let up = crate::fmath::ceil(self.font.ascender as f32 * s) as i32;
        let down = crate::fmath::ceil(-(self.font.descender as f32) * s) as i32;
        let gap = crate::fmath::round(self.font.line_gap as f32 * s) as i32;
        (up, down, up + down + gap)
    }

    /// How far `text` moves the pen at `px`, in whole pixels.
    pub fn measure(&mut self, px: f32, text: &str) -> i32 {
        let q6: i32 = text.chars().map(|c| self.glyph(c, px).advance_q6).sum();
        (q6 + 32) >> 6
    }
}

/// Which face of the chain draws `c`: the first that has it, else the first
/// (its missing-glyph box).
fn pick(faces: &[Face], c: char) -> usize {
    faces.iter().position(|f| f.has(c)).unwrap_or(0)
}

/// Whether some face of the chain draws `c`.
pub fn covers(faces: &[Face], c: char) -> bool {
    faces.iter().any(|f| f.has(c))
}

/// How far `text` moves the pen at `px` with this chain, in whole pixels.
pub fn measure(faces: &mut [Face], px: f32, text: &str) -> i32 {
    let q6: i32 = text.chars().map(|c| faces[pick(faces, c)].glyph(c, px).advance_q6).sum();
    (q6 + 32) >> 6
}

/// Draws `text` at `px` from the chain `faces`, its pen starting at `x` on
/// the baseline `y`, in `rgb`; returns the pen's end. Glyphs sit on whole
/// pixels; the pen keeps its 1/64s, so a run is as wide as the fonts say.
pub fn draw(t: &mut impl Target, faces: &mut [Face], px: f32, x: i32, y: i32, text: &str, rgb: u32) -> i32 {
    let (w, h) = t.size();
    let mut pen = x << 6;
    for c in text.chars() {
        let i = pick(faces, c);
        let g = faces[i].glyph(c, px);
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
                if a != 0 {
                    let dst = t.get(px as usize, py as usize);
                    t.set(px as usize, py as usize, blend(dst, rgb, a));
                }
            }
        }
        pen += g.advance_q6;
    }
    (pen + 32) >> 6
}

/// A terminal's character cell (TASK-054): whole pixels, so a grid of them
/// never drifts. Width is the monospace face's advance; height its ascent
/// plus descent plus line gap; the baseline sits `baseline` px down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub w: i32,
    pub h: i32,
    pub baseline: i32,
}

/// The cell of a monospace `face` at `px`.
pub fn cell(face: &mut Face, px: f32) -> Cell {
    let w = (face.glyph('M', px).advance_q6 + 32) >> 6;
    let (up, down, line) = face.metrics(px);
    Cell { w: w.max(1), h: line.max(up + down), baseline: up }
}

/// Draws `c` in the cell at column `col`, row `row` of a grid whose top-left
/// is (`x`, `y`): the glyph on the cell's baseline, wherever the previous
/// cells' fractions would have put it.
#[allow(clippy::too_many_arguments)]
pub fn draw_cell(
    t: &mut impl Target,
    faces: &mut [Face],
    px: f32,
    cell: Cell,
    x: i32,
    y: i32,
    col: i32,
    row: i32,
    c: char,
    rgb: u32,
) {
    let mut s = [0u8; 4];
    draw(t, faces, px, x + col * cell.w, y + row * cell.h + cell.baseline, c.encode_utf8(&mut s), rgb);
}
