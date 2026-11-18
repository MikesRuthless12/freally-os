// SPDX-License-Identifier: Apache-2.0
//! The fleet cursor (TASK-058, FR-029), as the cursor kit
//! (`Freally Bugs/cursor-kit`) defines it, drawn by stage from its first
//! frame, so the system arrow never shows:
//!
//! - the dot (6 px) is the hotspot, exactly at the pointer;
//! - the ring (34 px across, a 1.5 px line) trails it, time based (70 ms),
//!   never so far that the dot leaves it: moving down-left, the dot rides
//!   in the ring's lower left;
//! - over something to press (a window's Hand, stage's close button) the
//!   ring grows ×1.45 with a faint fill (14 %); pressed it shrinks ×0.78,
//!   and comes back no sooner than 140 ms, so a quick click still shows;
//!   sizes ease over 160 ms (90 ms into a press);
//! - at rest it costs nothing: frames run only while something moves;
//! - Reduce Motion drops the trail and the easing.
//!
//! Every other kind (text, the resizes, move, crosshair, grab, not allowed)
//! is drawn as itself, and the ring and dot step aside.

use crate::windows::Rect;
use fimage::blit::Surface;
use fui::color::Rgba;
use fui::draw;

pub const RING: f32 = 34.0;
pub const LINE: f32 = 1.5;
pub const DOT: f32 = 6.0;
pub const HOVER: f32 = 1.45;
pub const PRESS: f32 = 0.78;
pub const FOLLOW_MS: f32 = 70.0;
pub const PRESS_MIN_MS: u64 = 140;
pub const EASE_MS: f32 = 160.0;
pub const PRESS_EASE_MS: f32 = 90.0;
/// The hover fill's strength.
pub const FILL: f32 = 0.14;

/// What a window asks the pointer to look like over it (PRD § 4, `set_cursor`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Arrow,
    Hand,
    Text,
    ResizeEw,
    ResizeNs,
    ResizeNwse,
    ResizeNesw,
    Move,
    Crosshair,
    Grab,
    NotAllowed,
}

impl Kind {
    pub fn from_u8(v: u8) -> Option<Kind> {
        use Kind::*;
        [Arrow, Hand, Text, ResizeEw, ResizeNs, ResizeNwse, ResizeNesw, Move, Crosshair, Grab, NotAllowed]
            .get(v as usize)
            .copied()
    }

    /// The ring and dot stand for the arrow and the hand only.
    pub fn is_ring(self) -> bool {
        matches!(self, Kind::Arrow | Kind::Hand)
    }
}

/// e^-x for x ≥ 0, without libm: 2^-n by its bits, the rest by a series.
pub fn exp_neg(x: f32) -> f32 {
    if x <= 0.0 {
        return 1.0;
    }
    if x > 80.0 {
        return 0.0;
    }
    const LN2: f32 = core::f32::consts::LN_2;
    let n = (x / LN2) as i32;
    let r = x - n as f32 * LN2;
    // e^-r for r in [0, ln 2): seven terms are exact to f32.
    let mut term = 1.0f32;
    let mut sum = 1.0f32;
    for k in 1..8 {
        term *= -r / k as f32;
        sum += term;
    }
    sum * f32::from_bits(((127 - n) as u32) << 23)
}

/// How far the ring's centre may be from the dot at `scale`: the dot stays inside.
pub fn reach(scale: f32) -> f32 {
    (RING * scale / 2.0 - DOT / 2.0 - LINE).max(0.0)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cursor {
    /// The hotspot: where the pointer is.
    pub dot: (f32, f32),
    /// The ring's centre.
    pub ring: (f32, f32),
    /// The ring's size now (1 at rest), easing toward `target()`.
    pub scale: f32,
    pub kind: Kind,
    pub hover: bool,
    /// When the press began (ns), while it's held or showing.
    pressed_at: Option<u64>,
    held: bool,
    pub reduce_motion: bool,
    last_ns: u64,
}

impl Cursor {
    pub fn new(x: i32, y: i32, now_ns: u64) -> Cursor {
        let p = (x as f32, y as f32);
        Cursor {
            dot: p,
            ring: p,
            scale: 1.0,
            kind: Kind::Arrow,
            hover: false,
            pressed_at: None,
            held: false,
            reduce_motion: false,
            last_ns: now_ns,
        }
    }

    /// The size the ring is heading for.
    pub fn target(&self, now_ns: u64) -> f32 {
        let grow = if self.hover { HOVER } else { 1.0 };
        let pressed = self.held || self.pressed_at.is_some_and(|t| now_ns < t + PRESS_MIN_MS * 1_000_000);
        grow * if pressed { PRESS } else { 1.0 }
    }

    pub fn move_to(&mut self, x: i32, y: i32) {
        self.dot = (x as f32, y as f32);
    }

    pub fn press(&mut self, down: bool, now_ns: u64) {
        if down {
            self.pressed_at = Some(now_ns);
        }
        self.held = down;
    }

    /// Advances to `now_ns`: the ring follows the dot, the size eases.
    /// Returns whether anything is still moving (more frames wanted).
    pub fn tick(&mut self, now_ns: u64) -> bool {
        let dt = now_ns.saturating_sub(self.last_ns) as f32 / 1e6;
        self.last_ns = now_ns;
        let target = self.target(now_ns);
        if self.reduce_motion {
            self.ring = self.dot;
            self.scale = target;
        } else {
            let k = 1.0 - exp_neg(dt / FOLLOW_MS);
            self.ring.0 += (self.dot.0 - self.ring.0) * k;
            self.ring.1 += (self.dot.1 - self.ring.1) * k;
            let ease = if target < self.scale { PRESS_EASE_MS } else { EASE_MS };
            // An exponential ease that's within a twentieth of a pixel in its time.
            let e = 1.0 - exp_neg(dt * 3.0 / ease);
            self.scale += (target - self.scale) * e;
        }
        // Never so far that the dot leaves the ring.
        let (dx, dy) = (self.ring.0 - self.dot.0, self.ring.1 - self.dot.1);
        let d2 = dx * dx + dy * dy;
        let r = reach(self.scale);
        if d2 > r * r {
            let d = sqrt(d2);
            self.ring = (self.dot.0 + dx * r / d, self.dot.1 + dy * r / d);
        }
        if (self.scale - target).abs() < 0.002 {
            self.scale = target;
        }
        if self.ring_settled() {
            self.ring = self.dot;
        }
        if !self.held && self.pressed_at.is_some_and(|t| now_ns >= t + PRESS_MIN_MS * 1_000_000) {
            self.pressed_at = None;
        }
        self.moving(now_ns)
    }

    fn ring_settled(&self) -> bool {
        (self.ring.0 - self.dot.0).abs() < 0.25 && (self.ring.1 - self.dot.1).abs() < 0.25
    }

    /// Whether frames are still wanted: the ring catching up, the size
    /// easing, or a press waiting out its 140 ms.
    pub fn moving(&self, now_ns: u64) -> bool {
        !self.ring_settled() || self.scale != self.target(now_ns) || (self.pressed_at.is_some() && !self.held)
    }

    /// Everything the cursor may cover, whole pixels with a margin for the
    /// anti-aliased edge.
    pub fn bounds(&self) -> Rect {
        let r = RING * self.scale.max(HOVER) / 2.0 + 2.0;
        let (x0, y0) = ((self.ring.0.min(self.dot.0) - r) as i32 - 1, (self.ring.1.min(self.dot.1) - r) as i32 - 1);
        let (x1, y1) = ((self.ring.0.max(self.dot.0) + r) as i32 + 2, (self.ring.1.max(self.dot.1) + r) as i32 + 2);
        Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32)
    }
}

/// The colours the cursor is drawn in: the ring and dot in the theme's
/// primary ink; the drawn kinds in the text colour, edged in the ground's
/// so they read on anything; not-allowed in the error red.
#[derive(Clone, Copy, Debug)]
pub struct Inks {
    pub ring: Rgba,
    pub glyph: Rgba,
    pub edge: Rgba,
    pub stop: Rgba,
}

/// Draws the cursor onto `s`, the screen's pixels under it, `s` standing at
/// `origin` on the screen.
pub fn draw(c: &Cursor, s: &mut Surface, origin: (i32, i32), inks: Inks) {
    let at = |p: (f32, f32)| (p.0 - origin.0 as f32 + 0.5, p.1 - origin.1 as f32 + 0.5);
    if c.kind.is_ring() {
        let (rx, ry) = at(c.ring);
        let outer = RING * c.scale / 2.0;
        if c.hover {
            fill_srgb(s, (rx, ry), outer - LINE, inks.ring, FILL);
        }
        draw::arc(s, (rx, ry), outer - LINE / 2.0, LINE, (0.0, core::f32::consts::TAU), inks.ring);
        let (dx, dy) = at(c.dot);
        draw::disc(s, dx, dy, DOT / 2.0, inks.ring);
        return;
    }
    let (x, y) = at(c.dot);
    let arrow = |len: f32, ux: f32, uy: f32| -> [(f32, f32, f32, f32); 5] {
        // A double-headed arrow along (ux, uy), heads 4 px.
        let (px, py) = (-uy, ux);
        let (ex, ey) = (ux * len, uy * len);
        [
            (-ex, -ey, ex, ey),
            (ex, ey, ex - 4.0 * ux + 4.0 * px, ey - 4.0 * uy + 4.0 * py),
            (ex, ey, ex - 4.0 * ux - 4.0 * px, ey - 4.0 * uy - 4.0 * py),
            (-ex, -ey, -ex + 4.0 * ux + 4.0 * px, -ey + 4.0 * uy + 4.0 * py),
            (-ex, -ey, -ex + 4.0 * ux - 4.0 * px, -ey + 4.0 * uy - 4.0 * py),
        ]
    };
    const D: f32 = core::f32::consts::FRAC_1_SQRT_2;
    match c.kind {
        Kind::Text => stroke(
            s,
            (x, y),
            inks.edge,
            &[(0.0, -9.0, 0.0, 9.0), (-3.5, -9.0, 3.5, -9.0), (-3.5, 9.0, 3.5, 9.0)],
            inks.glyph,
        ),
        Kind::ResizeEw => stroke(s, (x, y), inks.edge, &arrow(10.0, 1.0, 0.0), inks.glyph),
        Kind::ResizeNs => stroke(s, (x, y), inks.edge, &arrow(10.0, 0.0, 1.0), inks.glyph),
        Kind::ResizeNwse => stroke(s, (x, y), inks.edge, &arrow(9.0, D, D), inks.glyph),
        Kind::ResizeNesw => stroke(s, (x, y), inks.edge, &arrow(9.0, D, -D), inks.glyph),
        Kind::Move => {
            let mut all = arrow(10.0, 1.0, 0.0).to_vec();
            all.extend(arrow(10.0, 0.0, 1.0));
            stroke(s, (x, y), inks.edge, &all, inks.glyph);
        }
        Kind::Crosshair => stroke(
            s,
            (x, y),
            inks.edge,
            &[(-11.0, 0.0, -3.0, 0.0), (3.0, 0.0, 11.0, 0.0), (0.0, -11.0, 0.0, -3.0), (0.0, 3.0, 0.0, 11.0)],
            inks.glyph,
        ),
        Kind::Grab => {
            draw::arc(s, (x, y), 7.0, 4.0, (0.0, core::f32::consts::TAU), inks.edge);
            draw::arc(s, (x, y), 7.0, 1.75, (0.0, core::f32::consts::TAU), inks.glyph);
            draw::disc(s, x, y, 2.0, inks.glyph);
        }
        Kind::NotAllowed => {
            draw::arc(s, (x, y), 8.0, 4.5, (0.0, core::f32::consts::TAU), inks.edge);
            draw::arc(s, (x, y), 8.0, 2.0, (0.0, core::f32::consts::TAU), inks.stop);
            stroke(s, (x, y), inks.edge, &[(-5.5, -5.5, 5.5, 5.5)], inks.stop);
        }
        Kind::Arrow | Kind::Hand => {}
    }
}

/// The hover's faint fill: `ink` at `strength` over what's there, mixed in
/// sRGB as the kit's `color-mix(in srgb, …)` is, so it's as faint as every
/// other Freally app's (linear light would show it twice as strong).
fn fill_srgb(s: &mut Surface, (cx, cy): (f32, f32), rad: f32, ink: Rgba, strength: f32) {
    let (x0, x1) = ((cx - rad - 1.0).max(0.0) as u32, ((cx + rad + 1.0) as u32).min(s.width));
    let (y0, y1) = ((cy - rad - 1.0).max(0.0) as u32, ((cy + rad + 1.0) as u32).min(s.height));
    for y in y0..y1 {
        for x in x0..x1 {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let k = (0.5 - (sqrt(dx * dx + dy * dy) - rad)).clamp(0.0, 1.0) * strength;
            if k <= 0.0 {
                continue;
            }
            let i = (y * s.stride + x) as usize;
            let d = s.px[i];
            let ch = |sh: u32, v: u8| {
                let a = (d >> sh & 0xFF) as f32;
                ((a + (v as f32 - a) * k + 0.5) as u32).min(255) << sh
            };
            s.px[i] = ch(16, ink.r) | ch(8, ink.g) | ch(0, ink.b);
        }
    }
}

/// Lines from (x, y), each twice: the edge, wide, then the glyph inside it.
fn stroke(s: &mut Surface, (x, y): (f32, f32), edge: Rgba, lines: &[(f32, f32, f32, f32)], ink: Rgba) {
    for (w, colour) in [(4.0, edge), (1.75, ink)] {
        for &(x0, y0, x1, y1) in lines {
            draw::line(s, x + x0, y + y0, x + x1, y + y1, w, colour);
        }
    }
}

fn sqrt(v: f32) -> f32 {
    if v <= 0.0 {
        return 0.0;
    }
    let mut x = if v > 1.0 { v / 2.0 } else { 1.0 };
    for _ in 0..12 {
        x = 0.5 * (x + v / x);
    }
    x
}

#[cfg(test)]
mod tests;
