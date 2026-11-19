// SPDX-License-Identifier: Apache-2.0
//! THE FLEET DISPLAY, in the OS (TASK-075A; the fleet rule of 2026-11-16):
//! the Freally plot kit's pen (`Freally Bugs/plot-kit/kit/core/plot.js`),
//! ported, because the kit is JavaScript and the OS draws its own pixels.
//! Its numbers are the test: each function here answers what the kit's does
//! for the same data (the figures in `tests` came from running the kit).
//!
//! - **Shapes filled with light:** the accent, strong where the value is
//!   high and gone by the foot, by LEVEL (the plot's own height), never by
//!   a shape's own box (`fill_under`, the `body` ink).
//! - **Curves smooth:** a monotone cubic through every point that never
//!   overshoots between two (`smooth_through`, Fritsch–Carlson). The pen is
//!   smoothed, never the data.
//! - **Lines lit and crisp:** whole device pixels (`crisp`), a soft glow in
//!   the dark theme and none in the light (`stroke`).
//! - **A calibrated grid** behind it (`grid`), its figures in the mono face
//!   (the caller writes them).
//! - **Only what the data says:** fewer than two points draw nothing; the
//!   caller says it's waiting.
//!
//! The kit is never edited from here: a piece the pen lacks lands in the
//! kit first.

use crate::color::Rgba;
use crate::layout::Rect;
use alloc::vec::Vec;
use fimage::blit::Surface;

/// A point, in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pt {
    pub x: f64,
    pub y: f64,
}

/// One Bézier segment after the curve's last point: its two controls and its end.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seg {
    pub c1: Pt,
    pub c2: Pt,
    pub to: Pt,
}

/// floor and ceil, by hand: Freally's maths has no libm.
fn floor(v: f64) -> f64 {
    let t = v as i64 as f64;
    if t > v { t - 1.0 } else { t }
}

fn ceil(v: f64) -> f64 {
    -floor(-v)
}

fn sqrt(v: f64) -> f64 {
    if v <= 0.0 {
        return 0.0;
    }
    let mut x = if v > 1.0 { v / 2.0 } else { 1.0 };
    for _ in 0..40 {
        x = 0.5 * (x + v / x);
    }
    x
}

/// A SMOOTH CURVE THROUGH EVERY POINT (the kit's `smoothThrough`): monotone
/// cubic interpolation, Fritsch–Carlson, as the Bézier segments that follow
/// the first point. Flat where the curve turns, so a peak stays a peak; the
/// limiter keeps each span between its two points. Fewer than two: none.
pub fn smooth_through(points: &[Pt]) -> Vec<Seg> {
    let n = points.len();
    if n < 2 {
        return Vec::new();
    }
    let mut run = Vec::with_capacity(n - 1);
    let mut slope = Vec::with_capacity(n - 1);
    for i in 0..n - 1 {
        let dx = points[i + 1].x - points[i].x;
        run.push(dx);
        slope.push(if dx == 0.0 { 0.0 } else { (points[i + 1].y - points[i].y) / dx });
    }
    let mut tangent: Vec<f64> = (0..n)
        .map(|i| {
            if i == 0 {
                slope[0]
            } else if i == n - 1 {
                slope[n - 2]
            } else if slope[i - 1] * slope[i] <= 0.0 {
                0.0
            } else {
                (slope[i - 1] + slope[i]) / 2.0
            }
        })
        .collect();
    for i in 0..n - 1 {
        let here = slope[i];
        if here == 0.0 {
            tangent[i] = 0.0;
            tangent[i + 1] = 0.0;
            continue;
        }
        let (a, b) = (tangent[i] / here, tangent[i + 1] / here);
        let size = a * a + b * b;
        if size > 9.0 {
            let k = 3.0 / sqrt(size);
            tangent[i] = k * a * here;
            tangent[i + 1] = k * b * here;
        }
    }
    (0..n - 1)
        .map(|i| {
            let (from, to) = (points[i], points[i + 1]);
            let third = run[i] / 3.0;
            Seg {
                c1: Pt { x: from.x + third, y: from.y + tangent[i] * third },
                c2: Pt { x: to.x - third, y: to.y - tangent[i + 1] * third },
                to,
            }
        })
        .collect()
}

/// The curve as straight pieces, `steps` per segment (for filling and stroking).
pub fn flatten(points: &[Pt], steps: usize) -> Vec<Pt> {
    let Some(&first) = points.first() else { return Vec::new() };
    let mut out = alloc::vec![first];
    let mut from = first;
    for s in smooth_through(points) {
        for k in 1..=steps.max(1) {
            let t = k as f64 / steps.max(1) as f64;
            let u = 1.0 - t;
            let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            out.push(Pt {
                x: a * from.x + b * s.c1.x + c * s.c2.x + d * s.to.x,
                y: a * from.y + b * s.c1.y + c * s.c2.y + d * s.to.y,
            });
        }
        from = s.to;
    }
    out
}

/// A decay read through a wider window, AFTER its loudest value (the kit's
/// `steadied`): each later value the mean of itself and `reach` neighbours
/// either side, nearer ones weighted more, never reaching back over the peak.
pub fn steadied(values: &[f64], reach: usize) -> Vec<f64> {
    if reach == 0 || values.len() < 3 {
        return values.to_vec();
    }
    let peak = values.iter().enumerate().fold(0, |best, (i, v)| if *v > values[best] { i } else { best });
    (0..values.len())
        .map(|at| {
            if at <= peak {
                return values[at];
            }
            let span = reach.min(at - peak - 1).min(values.len() - 1 - at) as i64;
            if span <= 0 {
                return values[at];
            }
            let (mut sum, mut weights) = (0.0, 0.0);
            for step in -span..=span {
                let w = (span + 1 - step.abs()) as f64;
                sum += values[(at as i64 + step) as usize] * w;
                weights += w;
            }
            sum / weights
        })
        .collect()
}

/// A 1 px line on whole device pixels: `value` moved to the middle of the
/// device pixel it falls in (the kit's `crisp`).
pub fn crisp(value: f64, ratio: f64) -> f64 {
    let scale = if ratio > 0.0 { ratio } else { 1.0 };
    (((value * scale) as i64) as f64 + 0.5) / scale
}

/// A meter's cell, px, and the gap above it (the kit's).
pub const METER_CELL: u32 = 4;
pub const METER_GAP: u32 = 2;
/// How far down a level meter reads, dBFS.
pub const METER_FLOOR_DB: f64 = -60.0;

/// How many cells a ladder `height` tall holds, and how many `share` (0..=1)
/// lights: anything at all lights the first (a little is never nothing).
pub fn ladder_cells(height: u32, share: Option<f64>) -> (u32, u32) {
    let count = ((height + METER_GAP) / (METER_CELL + METER_GAP)).max(1);
    let held = share.filter(|s| s.is_finite()).unwrap_or(0.0).clamp(0.0, 1.0);
    let lit = if held <= 0.0 { 0 } else { ((held * count as f64 + 0.5) as u32).max(1) };
    (count, lit)
}

/// A level meter's cells: `db` dBFS on 0 to `METER_FLOOR_DB`.
pub fn meter_cells(height: u32, db: Option<f64>) -> (u32, u32) {
    ladder_cells(height, db.filter(|d| d.is_finite()).map(|d| (d - METER_FLOOR_DB) / -METER_FLOOR_DB))
}

/// Where cell `at` (0 at the foot) of a ladder `height` tall is drawn: its top, px.
pub fn ladder_cell_y(height: u32, at: u32) -> i32 {
    height as i32 - ((at + 1) * (METER_CELL + METER_GAP)) as i32 + METER_GAP as i32
}

/// The inks that fall from a plot's top to its foot (the kit's `INK_STOPS`):
/// the accent's strength at the top, at `INK_MID` of the way, at the foot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ink {
    /// The main shapes.
    Body,
    /// A shape under them.
    Soft,
}

pub const INK_MID: f64 = 0.55;

impl Ink {
    pub const fn stops(self) -> (f64, f64, f64) {
        match self {
            Ink::Body => (0.72, 0.4, 0.1),
            Ink::Soft => (0.36, 0.2, 0.05),
        }
    }

    /// The accent's strength at height `y`, by level between `top` and `foot`.
    pub fn at(self, y: f64, top: f64, foot: f64) -> f64 {
        let (t, m, f) = self.stops();
        let share = if foot > top { ((y - top) / (foot - top)).clamp(0.0, 1.0) } else { 0.0 };
        if share <= INK_MID { t + (m - t) * share / INK_MID } else { m + (f - m) * (share - INK_MID) / (1.0 - INK_MID) }
    }
}

fn blend(s: &mut Surface, x: i32, y: i32, c: Rgba, alpha: f64) {
    if x < 0 || y < 0 || x >= s.width as i32 || y >= s.height as i32 || alpha <= 0.0 {
        return;
    }
    let i = y as usize * s.stride as usize + x as usize;
    s.px[i] = ffont::text::blend(s.px[i], c.xrgb(), (alpha.min(1.0) * 255.0 + 0.5) as u8);
}

/// THE LIGHT UNDER A CURVE: from the smooth curve through `points` down to
/// `foot`, each row the `ink`'s strength at its level between `top` and
/// `foot` (so two shapes at one height wear one tone); the edge anti-aliased.
pub fn fill_under(s: &mut Surface, points: &[Pt], top: f64, foot: f64, accent: Rgba, ink: Ink) {
    let line = flatten(points, 12);
    if line.len() < 2 {
        return;
    }
    let (x0, x1) = (floor(line[0].x) as i32, ceil(line[line.len() - 1].x) as i32);
    let mut k = 0;
    for x in x0..x1 {
        let cx = x as f64 + 0.5;
        while k + 1 < line.len() - 1 && line[k + 1].x < cx {
            k += 1;
        }
        let (a, b) = (line[k], line[k + 1]);
        let t = if b.x > a.x { ((cx - a.x) / (b.x - a.x)).clamp(0.0, 1.0) } else { 0.0 };
        let y = a.y + (b.y - a.y) * t;
        let first = floor(y) as i32;
        for row in first..ceil(foot) as i32 {
            let cover = if row == first { 1.0 - (y - first as f64) } else { 1.0 };
            blend(s, x, row, accent, cover * ink.at(row as f64 + 0.5, top, foot));
        }
    }
}

/// THE LIT LINE: the smooth curve through `points`, `width` px, with a soft
/// glow under it in the dark theme (none in the light).
pub fn stroke(s: &mut Surface, points: &[Pt], width: f64, accent: Rgba, dark: bool) {
    let line = flatten(points, 12);
    for w in line.windows(2) {
        let (a, b) = (w[0], w[1]);
        if dark {
            let glow = Rgba { a: 40, ..accent };
            crate::draw::line(s, a.x as f32, a.y as f32, b.x as f32, b.y as f32, (width * 4.0) as f32, glow);
        }
    }
    for w in line.windows(2) {
        let (a, b) = (w[0], w[1]);
        crate::draw::line(s, a.x as f32, a.y as f32, b.x as f32, b.y as f32, width as f32, accent);
    }
}

/// A CALIBRATED GRID: `rows` and `cols` divisions of `r`, each a 1 px line on
/// whole pixels, faint.
pub fn grid(s: &mut Surface, r: Rect, rows: u32, cols: u32, colour: Rgba) {
    for i in 0..=rows {
        let y = crisp(r.y as f64 + r.h as f64 * i as f64 / rows.max(1) as f64, 1.0) as i32;
        for x in r.x..r.x + r.w as i32 {
            blend(s, x, y, colour, colour.a as f64 / 255.0);
        }
    }
    for i in 0..=cols {
        let x = crisp(r.x as f64 + r.w as f64 * i as f64 / cols.max(1) as f64, 1.0) as i32;
        for y in r.y..r.y + r.h as i32 {
            blend(s, x, y, colour, colour.a as f64 / 255.0);
        }
    }
}

#[cfg(test)]
mod tests;
