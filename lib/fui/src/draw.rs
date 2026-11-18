// SPDX-License-Identifier: Apache-2.0
//! Anti-aliased shapes for the controls (TASK-056): rounded rectangles,
//! discs, rings, arcs and lines, each pixel covered by how much of it the
//! shape takes (a signed distance, one pixel of smoothing), blended in
//! linear light like the words, so a lit arc stays crisp on both grounds.

use crate::color::Rgba;
use crate::layout::Rect;
use fimage::blit::Surface;

fn blend(s: &mut Surface, x: i32, y: i32, c: Rgba, cover: f32) {
    if x < 0 || y < 0 || x >= s.width as i32 || y >= s.height as i32 || cover <= 0.0 {
        return;
    }
    let i = y as usize * s.stride as usize + x as usize;
    let a = (cover.min(1.0) * c.a as f32 + 0.5) as u8;
    s.px[i] = ffont::text::blend(s.px[i], c.xrgb(), a);
}

/// Coverage of a pixel at signed distance `d` (negative inside).
fn cover(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

fn sqrt(v: f32) -> f32 {
    // Newton from a good start: exact enough for a pixel's distance.
    if v <= 0.0 {
        return 0.0;
    }
    let mut x = if v > 1.0 { v / 2.0 } else { 1.0 };
    for _ in 0..12 {
        x = 0.5 * (x + v / x);
    }
    x
}

/// Fills `r` with corners of radius `rad`.
pub fn round_rect(s: &mut Surface, r: Rect, rad: f32, c: Rgba) {
    let (x0, y0, x1, y1) = (r.x as f32, r.y as f32, (r.x + r.w as i32) as f32, (r.y + r.h as i32) as f32);
    let rad = rad.min(r.w as f32 / 2.0).min(r.h as f32 / 2.0);
    for py in r.y..r.y + r.h as i32 {
        for px in r.x..r.x + r.w as i32 {
            let (cx, cy) = (px as f32 + 0.5, py as f32 + 0.5);
            // Distance to the rounded box: to the inner box, less the radius.
            let dx = (x0 + rad - cx).max(cx - (x1 - rad)).max(0.0);
            let dy = (y0 + rad - cy).max(cy - (y1 - rad)).max(0.0);
            let d = if dx > 0.0 || dy > 0.0 { sqrt(dx * dx + dy * dy) - rad } else { -1.0 };
            blend(s, px, py, c, cover(d));
        }
    }
}

/// A filled disc.
pub fn disc(s: &mut Surface, cx: f32, cy: f32, rad: f32, c: Rgba) {
    for py in (cy - rad - 1.0) as i32..=(cy + rad + 1.0) as i32 {
        for px in (cx - rad - 1.0) as i32..=(cx + rad + 1.0) as i32 {
            let (dx, dy) = (px as f32 + 0.5 - cx, py as f32 + 0.5 - cy);
            blend(s, px, py, c, cover(sqrt(dx * dx + dy * dy) - rad));
        }
    }
}

/// A ring `width` wide centred on radius `rad`, between angles `from` and
/// `to` (radians, counter-clockwise from 3 o'clock, `from` < `to`); the
/// whole ring when they're a full turn apart.
pub fn arc(s: &mut Surface, (cx, cy): (f32, f32), rad: f32, width: f32, (from, to): (f32, f32), c: Rgba) {
    let tau = core::f32::consts::TAU;
    let full = to - from >= tau - 1e-4;
    let reach = rad + width;
    for py in (cy - reach - 1.0) as i32..=(cy + reach + 1.0) as i32 {
        for px in (cx - reach - 1.0) as i32..=(cx + reach + 1.0) as i32 {
            let (dx, dy) = (px as f32 + 0.5 - cx, cy - (py as f32 + 0.5));
            let d = (sqrt(dx * dx + dy * dy) - rad).abs() - width / 2.0;
            if d > 1.0 {
                continue;
            }
            if !full {
                let mut a = atan2(dy, dx);
                while a < from {
                    a += tau;
                }
                if a > to {
                    continue;
                }
            }
            blend(s, px, py, c, cover(d));
        }
    }
}

/// A line `width` wide from (x0, y0) to (x1, y1), round-ended.
pub fn line(s: &mut Surface, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, c: Rgba) {
    let (vx, vy) = (x1 - x0, y1 - y0);
    let len2 = (vx * vx + vy * vy).max(1e-6);
    let pad = width + 1.0;
    for py in (y0.min(y1) - pad) as i32..=(y0.max(y1) + pad) as i32 {
        for px in (x0.min(x1) - pad) as i32..=(x0.max(x1) + pad) as i32 {
            let (wx, wy) = (px as f32 + 0.5 - x0, py as f32 + 0.5 - y0);
            let t = ((wx * vx + wy * vy) / len2).clamp(0.0, 1.0);
            let (ex, ey) = (wx - t * vx, wy - t * vy);
            blend(s, px, py, c, cover(sqrt(ex * ex + ey * ey) - width / 2.0));
        }
    }
}

/// The focus ring: 2 px of `--color-focus`, 2 px outside the control.
pub fn focus_ring(s: &mut Surface, r: Rect, rad: f32, c: Rgba) {
    let outer = Rect::new(r.x - 4, r.y - 4, r.w + 8, r.h + 8);
    let (x0, y0, x1, y1) =
        (outer.x as f32, outer.y as f32, (outer.x + outer.w as i32) as f32, (outer.y + outer.h as i32) as f32);
    let rad = rad + 4.0;
    for py in outer.y..outer.y + outer.h as i32 {
        for px in outer.x..outer.x + outer.w as i32 {
            let (cx, cy) = (px as f32 + 0.5, py as f32 + 0.5);
            let dx = (x0 + rad - cx).max(cx - (x1 - rad)).max(0.0);
            let dy = (y0 + rad - cy).max(cy - (y1 - rad)).max(0.0);
            let edge = if dx > 0.0 || dy > 0.0 {
                sqrt(dx * dx + dy * dy) - rad
            } else {
                (x0 - cx).max(cx - x1).max(y0 - cy).max(cy - y1)
            };
            // The band 0..2 px in from the outer edge.
            blend(s, px, py, c, cover((edge + 1.0).abs() - 1.0));
        }
    }
}

/// atan2, by a minimax polynomial (error under 0.0002 rad): Freally's maths
/// has no libm.
pub fn atan2(y: f32, x: f32) -> f32 {
    use core::f32::consts::{FRAC_PI_2, PI};
    if x == 0.0 && y == 0.0 {
        return 0.0;
    }
    let (ax, ay) = (x.abs(), y.abs());
    let a = ax.min(ay) / ax.max(ay);
    let s = a * a;
    let mut r = ((-0.046_496_47 * s + 0.159_314_22) * s - 0.327_622_76) * s * a + a;
    if ay > ax {
        r = FRAC_PI_2 - r;
    }
    if x < 0.0 {
        r = PI - r;
    }
    if y < 0.0 { -r } else { r }
}
