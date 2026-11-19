// SPDX-License-Identifier: Apache-2.0
//! Bounce: the F glides and bounces off the screen's edges, a fading glow
//! behind it (each frame keeps most of the last one's light), and a burst
//! of light when it meets a corner exactly (both edges in the same frame).

use super::{Frame, Rng, Saver, Sprite, add, fade, green};
use alloc::vec::Vec;

/// A spark of a corner's burst: where it is, where it's going (px/s), and
/// how long it has left (ms).
#[derive(Clone, Copy, Debug)]
struct Spark {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    life: u32,
}

pub struct Bounce {
    f: Sprite,
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    sparks: Vec<Spark>,
    rng: Rng,
    /// Corners met so far (the spec reads it back).
    pub corners: u32,
}

const BURST: usize = 160;
const SPARK_MS: u32 = 1400;

impl Bounce {
    pub fn new(w: u32, h: u32, f: &Sprite, seed: u64) -> Bounce {
        // A quarter of the screen's height, its shape kept.
        let fh = (h / 4).max(8);
        let fw = (f.w * fh / f.h.max(1)).max(8);
        let mut rng = Rng::new(seed);
        // About a sixth of the screen's width a second, at a slant.
        let speed = w as f32 / 6.0;
        Bounce {
            f: f.scaled(fw, fh),
            x: rng.unit() * (w - fw) as f32,
            y: rng.unit() * (h - fh) as f32,
            vx: speed,
            vy: speed * 0.71,
            sparks: Vec::new(),
            rng,
            corners: 0,
        }
    }

    /// Aims it into the top-left corner from (x, y) (the spec's corner).
    pub fn aim(&mut self, x: f32, y: f32) {
        (self.x, self.y) = (x, y);
        let s = self.vx.abs().max(1.0);
        (self.vx, self.vy) = (-s, -s * y / x.max(1.0));
    }

    fn burst(&mut self, cx: f32, cy: f32) {
        self.corners += 1;
        for _ in 0..BURST {
            let (s, c) = fui::controls::sin_cos(self.rng.unit() * core::f32::consts::TAU);
            let v = 80.0 + self.rng.unit() * 520.0;
            self.sparks.push(Spark { x: cx, y: cy, vx: c * v, vy: s * v, life: SPARK_MS });
        }
    }
}

impl Saver for Bounce {
    fn frame(&mut self, f: &mut Frame, dt_ms: u32) {
        let dt = dt_ms.min(100) as f32 / 1000.0;
        // The trail: what was, a little dimmer.
        fade(f, 200);
        let (maxx, maxy) = ((f.w - self.f.w) as f32, (f.h - self.f.h) as f32);
        self.x += self.vx * dt;
        self.y += self.vy * dt;
        let mut hit = (false, false);
        if self.x <= 0.0 || self.x >= maxx {
            self.vx = -self.vx;
            self.x = self.x.clamp(0.0, maxx);
            hit.0 = true;
        }
        if self.y <= 0.0 || self.y >= maxy {
            self.vy = -self.vy;
            self.y = self.y.clamp(0.0, maxy);
            hit.1 = true;
        }
        if hit == (true, true) {
            let cx = if self.x <= 0.0 { 0.0 } else { f.w as f32 };
            let cy = if self.y <= 0.0 { 0.0 } else { f.h as f32 };
            self.burst(cx, cy);
        }
        self.f.draw(f, self.x as i32, self.y as i32, 256);
        let dt_ms = dt_ms.min(100);
        self.sparks.retain_mut(|s| {
            s.x += s.vx * dt;
            s.y += s.vy * dt;
            s.life = s.life.saturating_sub(dt_ms);
            let level = 255 * s.life / SPARK_MS;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                add(f, s.x as i32 + dx, s.y as i32 + dy, green(level));
            }
            s.life > 0
        });
    }
}
