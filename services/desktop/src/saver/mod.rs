// SPDX-License-Identifier: Apache-2.0
//! The screensavers (TASK-077C, FR-091): drawn live by the OS's own code, on
//! the CPU (until the GPU driver, Phase 22), inside the desktop service.
//!
//! - **Bounce** ([`bounce`]): the F glides and bounces off the edges, a
//!   fading glow behind it, a burst of light when it meets a corner exactly;
//! - **The turning block** ([`block`]): the icon as a block of black glass
//!   with green edges, turning slowly, light sliding across its faces;
//! - **Swarm** ([`swarm`]): thousands of green sparks flock into the F,
//!   hold, burst apart and form it again;
//! - **Warp** ([`warp`]): flying through a tunnel of green light, Fs
//!   streaking past, one big F waiting at the end;
//! - Reduce Motion: the F still and dimmed ([`still`]).
//!
//! Every saver draws a whole opaque frame (nothing behind it ever shows) from
//! the time since the last one; the art is the owner's F, never redrawn.

pub mod block;
pub mod bounce;
pub mod still;
pub mod swarm;
pub mod warp;

use alloc::boxed::Box;
use alloc::vec::Vec;

/// Which saver (FR-091's order; Bounce is the default).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Bounce = 0,
    Block = 1,
    Swarm = 2,
    Warp = 3,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Bounce, Kind::Block, Kind::Swarm, Kind::Warp];

    pub fn from_u8(v: u8) -> Kind {
        Kind::ALL.get(v as usize).copied().unwrap_or(Kind::Bounce)
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Bounce => "Bounce",
            Kind::Block => "The turning block",
            Kind::Swarm => "Swarm",
            Kind::Warp => "Warp",
        }
    }
}

/// A frame to draw: opaque 0x00RRGGBB, `w × h`, rows tightly packed.
pub struct Frame<'a> {
    pub px: &'a mut [u32],
    pub w: u32,
    pub h: u32,
}

/// A picture the savers draw with: premultiplied (b, g, r, a), `w × h`.
#[derive(Clone, Debug)]
pub struct Sprite {
    pub w: u32,
    pub h: u32,
    pub px: Vec<[u8; 4]>,
}

impl Sprite {
    /// From FIMG pixels (premultiplied BGRA, rows tightly packed).
    pub fn from_bgra(w: u32, h: u32, bytes: &[u8]) -> Sprite {
        Sprite { w, h, px: bytes.as_chunks::<4>().0.to_vec() }
    }

    pub fn at(&self, x: u32, y: u32) -> [u8; 4] {
        self.px[(y.min(self.h - 1) * self.w + x.min(self.w - 1)) as usize]
    }

    /// The sprite at `w × h`, each new pixel the average of the old ones it
    /// covers (a box filter: no shimmer when it shrinks).
    pub fn scaled(&self, w: u32, h: u32) -> Sprite {
        let (w, h) = (w.max(1), h.max(1));
        let mut px = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            let (y0, y1) = (y * self.h / h, ((y + 1) * self.h / h).max(y * self.h / h + 1));
            for x in 0..w {
                let (x0, x1) = (x * self.w / w, ((x + 1) * self.w / w).max(x * self.w / w + 1));
                let mut s = [0u32; 4];
                for sy in y0..y1.min(self.h) {
                    for sx in x0..x1.min(self.w) {
                        let p = self.px[(sy * self.w + sx) as usize];
                        for c in 0..4 {
                            s[c] += p[c] as u32;
                        }
                    }
                }
                let n = ((y1.min(self.h) - y0) * (x1.min(self.w) - x0)).max(1);
                px.push(s.map(|v| (v / n) as u8));
            }
        }
        Sprite { w, h, px }
    }

    /// Draws it with its top-left at (x, y), its light scaled by `bright`
    /// (0–256: 256 as it is), over the frame.
    pub fn draw(&self, f: &mut Frame, x: i32, y: i32, bright: u32) {
        for sy in 0..self.h as i32 {
            let ty = y + sy;
            if ty < 0 || ty >= f.h as i32 {
                continue;
            }
            let row = (ty as u32 * f.w) as usize;
            for sx in 0..self.w as i32 {
                let tx = x + sx;
                if tx < 0 || tx >= f.w as i32 {
                    continue;
                }
                let [b, g, r, a] = self.px[(sy as u32 * self.w + sx as u32) as usize];
                if a == 0 {
                    continue;
                }
                let d = &mut f.px[row + tx as usize];
                let inv = 255 - (a as u32 * bright / 256).min(255);
                let mix = |s: u8, dv: u32| ((s as u32 * bright / 256) + dv * inv / 255).min(255);
                *d = mix(r, *d >> 16 & 0xFF) << 16 | mix(g, *d >> 8 & 0xFF) << 8 | mix(b, *d & 0xFF);
            }
        }
    }
}

/// Every pixel's light multiplied by `keep`/256: what's left of the frames
/// before (a trail, motion blur).
pub fn fade(f: &mut Frame, keep: u32) {
    for p in f.px.iter_mut() {
        let v = *p;
        let r = ((v >> 16 & 0xFF) * keep) >> 8;
        let g = ((v >> 8 & 0xFF) * keep) >> 8;
        let b = ((v & 0xFF) * keep) >> 8;
        *p = r << 16 | g << 8 | b;
    }
}

/// Light added at (x, y), clamped: sparks and streaks.
#[inline]
pub fn add(f: &mut Frame, x: i32, y: i32, rgb: u32) {
    if x < 0 || y < 0 || x >= f.w as i32 || y >= f.h as i32 {
        return;
    }
    let d = &mut f.px[(y as u32 * f.w + x as u32) as usize];
    let c = |s: u32| ((*d >> s & 0xFF) + (rgb >> s & 0xFF)).min(255) << s;
    *d = c(16) | c(8) | c(0);
}

/// A square root without libm: Newton's steps from the exponent halved
/// (exact to the float for the distances the savers measure).
pub fn sqrt(x: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    let mut y = f32::from_bits((x.to_bits() >> 1) + 0x1FC0_0000);
    for _ in 0..4 {
        y = 0.5 * (y + x / y);
    }
    y
}

/// Freally green, at `level` (0–255).
pub fn green(level: u32) -> u32 {
    let l = level.min(255);
    (l * 0x3a / 255) << 16 | l << 8 | (l * 0x4a / 255)
}

/// A small, fixed random source (xorshift): a saver looks alive, and a test
/// sees the same frames every run.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// In [0, 1).
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// In [-1, 1).
    pub fn signed(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }
}

/// One saver, drawing frame after frame.
pub trait Saver {
    /// The next frame, `dt_ms` after the last.
    fn frame(&mut self, f: &mut Frame, dt_ms: u32);
}

/// A saver for a `w × h` screen, drawn with the owner's F, its glass cut
/// away (the block lays it on glass of its own).
pub fn new(kind: Kind, w: u32, h: u32, f: &Sprite, seed: u64) -> Box<dyn Saver> {
    match kind {
        Kind::Bounce => Box::new(bounce::Bounce::new(w, h, f, seed)),
        Kind::Block => Box::new(block::Block::new(w, h, f)),
        Kind::Swarm => Box::new(swarm::Swarm::new(w, h, f, seed)),
        Kind::Warp => Box::new(warp::Warp::new(w, h, f, seed)),
    }
}

#[cfg(test)]
mod tests;
