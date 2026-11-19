// SPDX-License-Identifier: Apache-2.0
//! Warp: flying forward through a tunnel of green light. Streaks run along
//! the tunnel's walls toward the viewer, small Fs streak past, and one big F
//! waits at the end, its light breathing. A point at depth z, angle a and
//! radius r lands at the centre plus (cos a, sin a) × r × k / z.

use super::{Frame, Rng, Saver, Sprite, add, fade, green};
use alloc::vec::Vec;
use fui::controls::sin_cos;

pub const STREAKS: usize = 900;
const FS: usize = 6;
const NEAR: f32 = 0.15;
const FAR: f32 = 8.0;
/// Units a second toward the viewer.
const SPEED: f32 = 2.4;

#[derive(Clone, Copy, Debug)]
struct Star {
    /// The angle's sine and cosine, and the radius.
    s: f32,
    c: f32,
    r: f32,
    z: f32,
}

pub struct Warp {
    stars: Vec<Star>,
    fs: Vec<Star>,
    /// The F, prescaled: 16 px tall to a quarter of the screen.
    sizes: Vec<Sprite>,
    big: Sprite,
    rng: Rng,
    t_ms: u32,
    k: f32,
}

fn star(rng: &mut Rng, r0: f32, r1: f32, z: f32) -> Star {
    let (s, c) = sin_cos(rng.unit() * core::f32::consts::TAU);
    Star { s, c, r: r0 + (r1 - r0) * rng.unit(), z }
}

impl Warp {
    pub fn new(w: u32, h: u32, f: &Sprite, seed: u64) -> Warp {
        let mut rng = Rng::new(seed);
        let deep = |rng: &mut Rng, r0: f32, r1: f32| {
            let z = NEAR + (FAR - NEAR) * rng.unit();
            star(rng, r0, r1, z)
        };
        let stars = (0..STREAKS).map(|_| deep(&mut rng, 0.55, 1.0)).collect();
        let fs = (0..FS).map(|_| deep(&mut rng, 0.25, 0.7)).collect();
        let tall = |hh: u32| f.scaled((f.w * hh / f.h.max(1)).max(4), hh.max(4));
        let sizes = (0..8).map(|i| tall(16 << i)).take_while(|s| s.h <= h / 4 || s.h == 16).collect();
        Warp { stars, fs, sizes, big: tall(h * 22 / 100), rng, t_ms: 0, k: w.min(h) as f32 * 0.5 }
    }

    fn at(&self, s: &Star, z: f32, cx: f32, cy: f32) -> (f32, f32) {
        (cx + s.c * s.r * self.k / z, cy + s.s * s.r * self.k / z)
    }
}

/// A line of light added from (x0, y0) to (x1, y1), `level` at its head
/// (x1, y1) fading to a third at its tail.
fn streak(f: &mut Frame, (x0, y0): (f32, f32), (x1, y1): (f32, f32), level: u32) {
    let (dx, dy) = (x1 - x0, y1 - y0);
    let n = (dx.abs().max(dy.abs()) as u32).clamp(1, 400);
    for i in 0..=n {
        let t = i as f32 / n as f32;
        let l = level / 3 + (level - level / 3) * i / n;
        add(f, (x0 + dx * t) as i32, (y0 + dy * t) as i32, green(l));
    }
}

impl Saver for Warp {
    fn frame(&mut self, f: &mut Frame, dt_ms: u32) {
        let dt_ms = dt_ms.min(100);
        self.t_ms = self.t_ms.wrapping_add(dt_ms);
        let dt = dt_ms as f32 / 1000.0;
        let (cx, cy) = (f.w as f32 / 2.0, f.h as f32 / 2.0);
        fade(f, 140);
        // The F at the end, breathing.
        let breath = (sin_cos(self.t_ms as f32 / 1000.0 * 1.3).0 + 1.0) / 2.0;
        let (bx, by) = ((f.w as i32 - self.big.w as i32) / 2, (f.h as i32 - self.big.h as i32) / 2);
        self.big.draw(f, bx, by, 150 + (breath * 106.0) as u32);
        let step = SPEED * dt;
        for i in 0..self.stars.len() {
            let s = self.stars[i];
            let z = s.z - step;
            if z <= NEAR {
                self.stars[i] = star(&mut self.rng, 0.55, 1.0, FAR);
                continue;
            }
            // The streak's tail where it was three steps ago: speed shows.
            let tail = self.at(&s, (z + step * 3.0).min(FAR), cx, cy);
            let head = self.at(&s, z, cx, cy);
            let level = ((1.0 - z / FAR) * 255.0) as u32;
            streak(f, tail, head, level);
            self.stars[i].z = z;
        }
        for i in 0..self.fs.len() {
            let s = self.fs[i];
            let z = s.z - step;
            if z <= NEAR * 3.0 {
                self.fs[i] = star(&mut self.rng, 0.25, 0.7, FAR);
                continue;
            }
            self.fs[i].z = z;
            // The prescaled size nearest what depth asks for.
            let want = (0.18 * self.k / z) as u32;
            let Some(sp) = self.sizes.iter().min_by_key(|sp| sp.h.abs_diff(want)) else { continue };
            let (x, y) = self.at(&s, z, cx, cy);
            let level = ((1.0 - z / FAR) * 256.0) as u32;
            sp.draw(f, x as i32 - sp.w as i32 / 2, y as i32 - sp.h as i32 / 2, level.clamp(40, 256));
        }
    }
}
