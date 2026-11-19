// SPDX-License-Identifier: Apache-2.0
//! The turning block: the icon as a block of black glass, its edges lit
//! green, the F on every face, turning slowly in space while a band of light
//! slides across it. A small renderer of its own: each face that looks at
//! the camera is drawn through the inverse of its projective map (the unit
//! square to the face on screen), worked incrementally along each row, so
//! the F lies flat and true on the glass however the block turns.

use super::{Frame, Saver, Sprite};
use alloc::vec::Vec;
use fui::controls::sin_cos;

type V3 = [f32; 3];

/// The cube's faces: each as its top-left, top-right, bottom-right and
/// bottom-left corners seen from outside, so (u, v) runs over it from its
/// top-left and the F reads the right way round on every face.
const FACES: [[V3; 4]; 6] = [
    [[-1.0, 1.0, -1.0], [1.0, 1.0, -1.0], [1.0, -1.0, -1.0], [-1.0, -1.0, -1.0]],
    [[1.0, 1.0, 1.0], [-1.0, 1.0, 1.0], [-1.0, -1.0, 1.0], [1.0, -1.0, 1.0]],
    [[1.0, 1.0, -1.0], [1.0, 1.0, 1.0], [1.0, -1.0, 1.0], [1.0, -1.0, -1.0]],
    [[-1.0, 1.0, 1.0], [-1.0, 1.0, -1.0], [-1.0, -1.0, -1.0], [-1.0, -1.0, 1.0]],
    [[-1.0, 1.0, 1.0], [1.0, 1.0, 1.0], [1.0, 1.0, -1.0], [-1.0, 1.0, -1.0]],
    [[-1.0, -1.0, -1.0], [1.0, -1.0, -1.0], [1.0, -1.0, 1.0], [-1.0, -1.0, 1.0]],
];

/// Toward the light: upper left, on the camera's side (the camera looks
/// along +z).
const LIGHT: V3 = [-0.45, 0.6, -0.66];

pub struct Block {
    /// A face as it looks unlit: black glass, the F, the green edge.
    tex: Vec<u32>,
    tsize: u32,
    half: f32,
    t_ms: u32,
    /// Faces drawn in the last frame (the spec reads it back).
    pub faces_drawn: u32,
}

fn rot(p: V3, (sa, ca): (f32, f32), (sb, cb): (f32, f32)) -> V3 {
    // About y by a, then about x by b.
    let (x, z) = (p[0] * ca + p[2] * sa, -p[0] * sa + p[2] * ca);
    let (y, z) = (p[1] * cb - z * sb, p[1] * sb + z * cb);
    [x, y, z]
}

fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// The projective map from the unit square to the quad `q` (Heckbert):
/// x = (a u + b v + c) / (g u + h v + 1), y = (d u + e v + f) / (…).
fn square_to_quad(q: [(f32, f32); 4]) -> [f32; 8] {
    let [(x0, y0), (x1, y1), (x2, y2), (x3, y3)] = q;
    let (dx1, dx2, dx3) = (x1 - x2, x3 - x2, x0 - x1 + x2 - x3);
    let (dy1, dy2, dy3) = (y1 - y2, y3 - y2, y0 - y1 + y2 - y3);
    let den = dx1 * dy2 - dx2 * dy1;
    let (g, h) =
        if den.abs() < 1e-9 { (0.0, 0.0) } else { ((dx3 * dy2 - dx2 * dy3) / den, (dx1 * dy3 - dx3 * dy1) / den) };
    [x1 - x0 + g * x1, x3 - x0 + h * x3, x0, y1 - y0 + g * y1, y3 - y0 + h * y3, y0, g, h]
}

impl Block {
    pub fn new(w: u32, h: u32, f: &Sprite) -> Block {
        let half = w.min(h) as f32 * 0.2;
        // The texture about as big as a face shows: no blocks, no waste.
        let tsize = ((half * 2.0) as u32).clamp(64, 640);
        let fs = tsize * 7 / 10;
        let fsh = (f.h * fs / f.w.max(1)).max(1);
        let small = f.scaled(fs, fsh);
        let (ox, oy) = ((tsize - fs) / 2, (tsize - fsh) / 2);
        let edge = (tsize / 40).max(2);
        let mut tex = Vec::with_capacity((tsize * tsize) as usize);
        for y in 0..tsize {
            for x in 0..tsize {
                // Black glass, a breath lighter toward its top.
                let shade = 10 + 10 * (tsize - y) / tsize;
                let mut rgb = [shade, shade + 4, shade + 2];
                if (ox..ox + fs).contains(&x) && (oy..oy + fsh).contains(&y) {
                    let [b, g, r, a] = small.at(x - ox, y - oy);
                    let inv = 255 - a as u32;
                    rgb = [r as u32 + rgb[0] * inv / 255, g as u32 + rgb[1] * inv / 255, b as u32 + rgb[2] * inv / 255];
                }
                let d = x.min(y).min(tsize - 1 - x).min(tsize - 1 - y);
                if d < edge {
                    let k = 255 - 120 * d / edge;
                    rgb = [0x3a * k / 255, k, 0x4a * k / 255];
                }
                tex.push(rgb[0].min(255) << 16 | rgb[1].min(255) << 8 | rgb[2].min(255));
            }
        }
        Block { tex, tsize, half, t_ms: 0, faces_drawn: 0 }
    }

    fn face(&self, f: &mut Frame, q: [(f32, f32); 4], light: f32, band_at: f32) {
        let [a, b, c, d, e, ff, g, h] = square_to_quad(q);
        // The inverse map, as the adjugate of [[a b c] [d e f] [g h 1]].
        let m = [
            e - ff * h,
            c * h - b,
            b * ff - c * e,
            ff * g - d,
            a - c * g,
            c * d - a * ff,
            d * h - e * g,
            b * g - a * h,
            a * e - b * d,
        ];
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (x, y) in q {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        let (xa, xb) = ((x0 as i32).max(0), ((x1 as i32) + 1).min(f.w as i32));
        let (ya, yb) = ((y0 as i32).max(0), ((y1 as i32) + 1).min(f.h as i32));
        let ts = self.tsize as f32;
        let lit = (light * 256.0) as u32;
        for y in ya..yb {
            let (px, py) = (xa as f32 + 0.5, y as f32 + 0.5);
            let mut uu = m[0] * px + m[1] * py + m[2];
            let mut vv = m[3] * px + m[4] * py + m[5];
            let mut ww = m[6] * px + m[7] * py + m[8];
            let row = (y as u32 * f.w) as usize;
            for x in xa..xb {
                if ww.abs() > 1e-12 {
                    let (u, v) = (uu / ww, vv / ww);
                    if (0.0..1.0).contains(&u) && (0.0..1.0).contains(&v) {
                        let t = self.tex[((v * ts) as u32 * self.tsize + (u * ts) as u32) as usize];
                        // The band of light sliding across.
                        let s = (u * 0.7 + v * 0.3 - band_at) / 0.12;
                        let band = if s.abs() < 1.0 { ((1.0 - s * s) * 70.0) as u32 } else { 0 };
                        let ch = |sh: u32| ((((t >> sh & 0xFF) * lit) >> 8) + band).min(255) << sh;
                        f.px[row + x as usize] = ch(16) | ch(8) | ch(0);
                    }
                }
                uu += m[0];
                vv += m[3];
                ww += m[6];
            }
        }
    }
}

impl Saver for Block {
    fn frame(&mut self, f: &mut Frame, dt_ms: u32) {
        self.t_ms = self.t_ms.wrapping_add(dt_ms.min(100));
        let t = self.t_ms as f32 / 1000.0;
        f.px.fill(0);
        let ya = sin_cos(t * 0.35);
        let xa = sin_cos(0.45 + 0.25 * sin_cos(t * 0.21).0);
        let (cx, cy) = (f.w as f32 / 2.0, f.h as f32 / 2.0);
        let dist = 4.6;
        // One unit at the block's centre spans `half` px on screen.
        let k = self.half * dist;
        let lnorm = super::sqrt(dot(LIGHT, LIGHT));
        let band_at = (t * 0.25) % 1.8 - 0.2;
        self.faces_drawn = 0;
        for face in FACES {
            let p = face.map(|v| rot(v, ya, xa));
            // Its outward normal (right × down); drawn only if it faces the camera.
            let (e1, e2) = (
                [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]],
                [p[3][0] - p[0][0], p[3][1] - p[0][1], p[3][2] - p[0][2]],
            );
            let n = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
            let centre = [(p[0][0] + p[2][0]) / 2.0, (p[0][1] + p[2][1]) / 2.0, (p[0][2] + p[2][2]) / 2.0 + dist];
            if dot(n, centre) >= 0.0 {
                continue;
            }
            let nl = super::sqrt(dot(n, n)).max(1e-6);
            let diffuse = (dot(n, LIGHT) / (nl * lnorm)).max(0.0);
            let light = 0.45 + 0.75 * diffuse;
            let q = p.map(|v| {
                let z = v[2] + dist;
                (cx + v[0] * k / z, cy - v[1] * k / z)
            });
            self.face(f, q, light, band_at);
            self.faces_drawn += 1;
        }
    }
}
