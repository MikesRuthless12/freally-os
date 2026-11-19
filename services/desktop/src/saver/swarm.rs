// SPDX-License-Identifier: Apache-2.0
//! Swarm: thousands of green sparks flock together into the F, hold it,
//! burst apart, drift, and form it again. Each spark has a place in the F
//! (a point of the F's solid shape); a 12 s cycle pulls them to it, holds,
//! throws them out from the middle, and lets them wander.

use super::{Frame, Rng, Saver, Sprite, add, fade, green};
use alloc::vec::Vec;

pub const SPARKS: usize = 6000;
const CYCLE_MS: u32 = 12_000;
const GATHER_MS: u32 = 5_000;
const HOLD_MS: u32 = 8_000;
const BURST_MS: u32 = 9_500;

#[derive(Clone, Copy, Debug)]
struct Spark {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    tx: f32,
    ty: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Gather,
    Hold,
    Burst,
    Drift,
}

pub struct Swarm {
    sparks: Vec<Spark>,
    rng: Rng,
    t_ms: u32,
    burst_done: bool,
    centre: (f32, f32),
}

impl Swarm {
    pub fn new(w: u32, h: u32, f: &Sprite, seed: u64) -> Swarm {
        let fh = (h * 55 / 100).max(8);
        let fw = (f.w * fh / f.h.max(1)).max(8);
        let shape = f.scaled(fw, fh);
        let (ox, oy) = ((w - fw) as f32 / 2.0, (h - fh) as f32 / 2.0);
        // Where the F is solid (its glow left out).
        let solid: Vec<(f32, f32)> = (0..fh)
            .flat_map(|y| (0..fw).map(move |x| (x, y)))
            .filter(|&(x, y)| shape.at(x, y)[3] > 160)
            .map(|(x, y)| (ox + x as f32, oy + y as f32))
            .collect();
        let mut rng = Rng::new(seed);
        let sparks = (0..SPARKS)
            .map(|i| {
                let (tx, ty) = if solid.is_empty() {
                    (w as f32 / 2.0, h as f32 / 2.0)
                } else {
                    solid[(i * 7919 + (rng.next_u64() % 997) as usize) % solid.len()]
                };
                Spark { x: rng.unit() * w as f32, y: rng.unit() * h as f32, vx: 0.0, vy: 0.0, tx, ty }
            })
            .collect();
        Swarm { sparks, rng, t_ms: 0, burst_done: false, centre: (w as f32 / 2.0, h as f32 / 2.0) }
    }

    pub fn phase(&self) -> Phase {
        match self.t_ms % CYCLE_MS {
            t if t < GATHER_MS => Phase::Gather,
            t if t < HOLD_MS => Phase::Hold,
            t if t < BURST_MS => Phase::Burst,
            _ => Phase::Drift,
        }
    }

    /// How many sparks are within `r` px of their place in the F.
    pub fn in_place(&self, r: f32) -> usize {
        self.sparks.iter().filter(|s| (s.x - s.tx) * (s.x - s.tx) + (s.y - s.ty) * (s.y - s.ty) < r * r).count()
    }
}

impl Saver for Swarm {
    fn frame(&mut self, f: &mut Frame, dt_ms: u32) {
        let dt_ms = dt_ms.min(100);
        self.t_ms = self.t_ms.wrapping_add(dt_ms);
        let dt = dt_ms as f32 / 1000.0;
        let phase = self.phase();
        if phase != Phase::Burst {
            self.burst_done = false;
        }
        let (k, damp, noise) = match phase {
            Phase::Gather => (6.0, 3.2, 40.0),
            Phase::Hold => (40.0, 9.0, 6.0),
            Phase::Burst | Phase::Drift => (0.0, 0.6, 60.0),
        };
        let throw = phase == Phase::Burst && !self.burst_done;
        self.burst_done |= throw;
        fade(f, 170);
        let (cx, cy) = self.centre;
        for s in self.sparks.iter_mut() {
            if throw {
                let (dx, dy) = (s.x - cx, s.y - cy);
                let d = super::sqrt(dx * dx + dy * dy).max(1.0);
                let v = 500.0 + self.rng.unit() * 700.0;
                s.vx += dx / d * v;
                s.vy += dy / d * v;
            }
            let ax = k * (s.tx - s.x) - damp * s.vx + noise * self.rng.signed();
            let ay = k * (s.ty - s.y) - damp * s.vy + noise * self.rng.signed();
            s.vx += ax * dt;
            s.vy += ay * dt;
            s.x += s.vx * dt;
            s.y += s.vy * dt;
            let speed = (s.vx.abs() + s.vy.abs()).min(600.0);
            // Bright when held, brighter still when flying.
            let level = 190 + (speed / 600.0 * 65.0) as u32;
            add(f, s.x as i32, s.y as i32, green(level));
            add(f, s.x as i32 + 1, s.y as i32, green(level * 3 / 4));
            add(f, s.x as i32, s.y as i32 + 1, green(level * 3 / 4));
            add(f, s.x as i32 + 1, s.y as i32 + 1, green(level / 2));
        }
    }
}
