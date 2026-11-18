// SPDX-License-Identifier: Apache-2.0
//! An exact-area coverage rasterizer.
//!
//! Every edge is cut at pixel rows and pixel columns. A piece of edge inside one
//! pixel, running from `xa` to `xb` over a height `dy` (signed by direction),
//! covers `dy · (right edge − mean x)` of that pixel and the full `dy` of every
//! pixel to its right in the row. Stored as differences (this pixel gets the
//! partial area, the next one the rest), a running sum along the row turns the
//! differences into coverage. Non-zero winding, clamped to [0, 1].

use alloc::vec;
use alloc::vec::Vec;

/// A coverage buffer `w × h`, filled by [`Raster::line`] and [`Raster::quad`].
pub struct Raster {
    w: usize,
    h: usize,
    /// Per row, `w + 1` difference cells.
    acc: Vec<f32>,
}

impl Raster {
    pub fn new(w: usize, h: usize) -> Raster {
        Raster { w, h, acc: vec![0.0; (w + 1) * h] }
    }

    /// Adds a straight edge from `(x0, y0)` to `(x1, y1)`, in pixels, y down.
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) {
        if y0 == y1 {
            return; // a horizontal edge covers no height
        }
        let (dir, xa, ya, xb, yb) = if y0 < y1 { (1.0, x0, y0, x1, y1) } else { (-1.0, x1, y1, x0, y0) };
        let dxdy = (xb - xa) / (yb - ya);
        let first = crate::fmath::floor(ya.max(0.0)) as usize;
        let last = (crate::fmath::ceil(yb.min(self.h as f32)) as usize).min(self.h);
        for row in first..last {
            let top = ya.max(row as f32);
            let bottom = yb.min(row as f32 + 1.0);
            if bottom <= top {
                continue;
            }
            let xt = xa + (top - ya) * dxdy;
            let xbm = xa + (bottom - ya) * dxdy;
            self.row_piece(row, xt, xbm, (bottom - top) * dir);
        }
    }

    /// Cuts one row's piece of an edge (from `x0` to `x1`, height `dy`) at column boundaries.
    fn row_piece(&mut self, row: usize, x0: f32, x1: f32, dy: f32) {
        let span = (x1 - x0).abs();
        if span < 1e-6 {
            self.cell(row, crate::fmath::floor(x0), x0, dy);
            return;
        }
        let step: f32 = if x1 > x0 { 1.0 } else { -1.0 };
        let mut xs = x0;
        loop {
            let boundary = if step > 0.0 { crate::fmath::floor(xs) + 1.0 } else { crate::fmath::ceil(xs) - 1.0 };
            let xe = if (step > 0.0 && boundary >= x1) || (step < 0.0 && boundary <= x1) { x1 } else { boundary };
            let part = dy * (xe - xs).abs() / span;
            let col = crate::fmath::floor(xs.min(xe));
            self.cell(row, col, 0.5 * (xs + xe), part);
            if xe == x1 {
                break;
            }
            xs = xe;
        }
    }

    /// Adds a piece of edge inside pixel column `col` with mean x `xmid` and height `d`.
    fn cell(&mut self, row: usize, col: f32, xmid: f32, d: f32) {
        let base = row * (self.w + 1);
        if col < 0.0 {
            // Left of the buffer: it covers the whole row from x = 0.
            self.acc[base] += d;
            return;
        }
        let c = col as usize;
        if c >= self.w {
            return; // right of the buffer: covers nothing visible
        }
        let f = (xmid - col).clamp(0.0, 1.0);
        self.acc[base + c] += d * (1.0 - f);
        self.acc[base + c + 1] += d * f;
    }

    /// Adds a quadratic Bézier edge, flattened into a fixed number of lines
    /// chosen from its curvature (the same input always gives the same lines).
    pub fn quad(&mut self, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32)) {
        let ddx = p0.0 - 2.0 * p1.0 + p2.0;
        let ddy = p0.1 - 2.0 * p1.1 + p2.1;
        let dev = crate::fmath::sqrt(ddx * ddx + ddy * ddy);
        let n = (crate::fmath::ceil(crate::fmath::sqrt(dev * 4.0)) as usize).clamp(1, 64);
        let mut prev = p0;
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let mt = 1.0 - t;
            let p = (
                mt * mt * p0.0 + 2.0 * mt * t * p1.0 + t * t * p2.0,
                mt * mt * p0.1 + 2.0 * mt * t * p1.1 + t * t * p2.1,
            );
            self.line(prev.0, prev.1, p.0, p.1);
            prev = p;
        }
    }

    /// The coverage, 0–255 per pixel, row by row.
    pub fn finish(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.w * self.h];
        for row in 0..self.h {
            let mut sum = 0.0f32;
            for x in 0..self.w {
                sum += self.acc[row * (self.w + 1) + x];
                let a = sum.abs().min(1.0);
                out[row * self.w + x] = (a * 255.0 + 0.5) as u8;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn polygon(w: usize, h: usize, pts: &[(f32, f32)]) -> Vec<u8> {
        let mut r = Raster::new(w, h);
        for i in 0..pts.len() {
            let a = pts[i];
            let b = pts[(i + 1) % pts.len()];
            r.line(a.0, a.1, b.0, b.1);
        }
        r.finish()
    }

    fn area(cov: &[u8]) -> f32 {
        cov.iter().map(|&a| a as f32 / 255.0).sum()
    }

    #[test]
    fn an_aligned_square_is_solid_inside_and_empty_outside() {
        let c = polygon(10, 10, &[(2.0, 2.0), (6.0, 2.0), (6.0, 6.0), (2.0, 6.0)]);
        for y in 0..10 {
            for x in 0..10 {
                let inside = (2..6).contains(&x) && (2..6).contains(&y);
                assert_eq!(c[y * 10 + x], if inside { 255 } else { 0 }, "({x}, {y})");
            }
        }
    }

    #[test]
    fn a_half_pixel_offset_square_covers_its_area_exactly() {
        let c = polygon(10, 10, &[(2.5, 2.5), (6.5, 2.5), (6.5, 6.5), (2.5, 6.5)]);
        assert!((area(&c) - 16.0).abs() < 0.05, "{}", area(&c));
        assert_eq!(c[2 * 10 + 2], 64); // a quarter of the corner pixel
        assert_eq!(c[4 * 10 + 2], 128); // half of an edge pixel
    }

    #[test]
    fn a_triangle_covers_its_analytic_area() {
        // Right triangle with legs 7 and 5: area 17.5.
        let c = polygon(12, 12, &[(1.0, 1.0), (8.0, 1.0), (1.0, 6.0)]);
        assert!((area(&c) - 17.5).abs() < 0.1, "{}", area(&c));
    }

    #[test]
    fn winding_direction_does_not_matter() {
        let cw = polygon(10, 10, &[(1.3, 1.7), (8.2, 2.9), (4.4, 8.6)]);
        let ccw = polygon(10, 10, &[(4.4, 8.6), (8.2, 2.9), (1.3, 1.7)]);
        assert_eq!(cw, ccw);
    }

    #[test]
    fn shapes_hanging_off_the_buffer_are_clipped() {
        let c = polygon(4, 4, &[(-3.0, -3.0), (2.0, -3.0), (2.0, 2.0), (-3.0, 2.0)]);
        assert_eq!(c[0], 255);
        assert_eq!(c[1], 255);
        assert_eq!(c[2], 0);
        assert_eq!(c[2 * 4], 0);
    }

    #[test]
    fn a_circle_from_quads_covers_pi_r_squared() {
        // A circle of radius 10 from eight quadratic arcs (control points on the tangent lines).
        let (cx, cy, r) = (16.0f32, 16.0f32, 10.0f32);
        let mut ras = Raster::new(32, 32);
        let k = r / (core::f32::consts::FRAC_PI_8).cos();
        for i in 0..8 {
            let a0 = i as f32 * core::f32::consts::FRAC_PI_4;
            let am = a0 + core::f32::consts::FRAC_PI_8;
            let a1 = a0 + core::f32::consts::FRAC_PI_4;
            ras.quad(
                (cx + r * a0.cos(), cy + r * a0.sin()),
                (cx + k * am.cos(), cy + k * am.sin()),
                (cx + r * a1.cos(), cy + r * a1.sin()),
            );
        }
        let a = area(&ras.finish());
        // Quadratic arcs bulge slightly past the true circle (under 0.5% here).
        assert!((a - core::f32::consts::PI * 100.0).abs() < 2.0, "{a}");
    }
}
