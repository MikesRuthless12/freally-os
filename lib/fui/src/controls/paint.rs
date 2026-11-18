// SPDX-License-Identifier: Apache-2.0
//! How the controls are drawn (TASK-056): the hardware set's art, lit and
//! moved by the control's state; never redrawn here. What the art doesn't
//! hold is drawn from data with `draw`'s anti-aliased shapes: the knob's
//! green arc (the fleet's new knob), focus rings, the drawn toggles' colours.

use super::{KNOB_START, KNOB_SWEEP, Zone, knob_angle, meter_lit, meter_zone, sin_cos};
use crate::draw;
use crate::hardware::{Hardware, KeyLook, Lamp, geometry as g};
use crate::layout::Rect;
use crate::theme::{Theme, Tok};
use fimage::blit::Surface;
use fimage::fimg::Fimg;

/// Draws `f` centred on (cx, cy).
fn centred(s: &mut Surface, f: &Fimg, cx: f32, cy: f32) {
    s.draw(f, (cx - f.width as f32 / 2.0 + 0.5) as i32, (cy - f.height as f32 / 2.0 + 0.5) as i32);
}

/// The switch's size: its track's.
pub fn switch_size(hw: &Hardware) -> (u32, u32) {
    (hw.switch_track[0].width, hw.switch_track[0].height)
}

/// A switch at (x, y): its track lit or dark, the knob at the end it's set
/// to. The art keeps its own lit state (the fleet's toggle rule leaves it).
pub fn paint_switch(s: &mut Surface, hw: &Hardware, x: i32, y: i32, on: bool) {
    let track = &hw.switch_track[on as usize];
    s.draw(track, x, y);
    let end = hw.len(g::SWITCH_END);
    let cx = x as f32 + if on { track.width as f32 - end } else { end };
    let cy = y as f32 + track.height as f32 / 2.0;
    centred(s, &hw.switch_knob_shadow, cx, cy);
    centred(s, &hw.switch_knob, cx, cy);
}

/// The knob's size: one frame's.
pub fn knob_size(hw: &Hardware) -> u32 {
    hw.knob_body[0].width
}

/// The body frame for a value: the knurling turns with the knob, its 16
/// frames covering one 9° period, clockwise.
pub fn knob_frame(value: f32) -> usize {
    let turned = value.clamp(0.0, 1.0) * KNOB_SWEEP.to_degrees();
    ((turned % g::KNOB_PERIOD_DEG) / g::KNOB_STEP_DEG) as usize % g::KNOB_FRAMES
}

/// The fleet's knob at (x, y): shadow and body from the art, the dim track
/// and the lit arc (Freally green, from the start to the pointer) in the
/// ring around the body, and the art's green pointer turned to the value.
pub fn paint_knob(s: &mut Surface, hw: &Hardware, t: &Theme, x: i32, y: i32, value: f32) {
    let size = knob_size(hw) as f32;
    let half = size / 2.0;
    let (cx, cy) = (x as f32 + half, y as f32 + half);
    s.draw(&hw.knob_shadow, x, y);
    s.draw(&hw.knob_body[knob_frame(value)], x, y);
    let ring = half * (1.0 + g::KNOB_BODY_SHARE) / 2.0;
    let width = half * (1.0 - g::KNOB_BODY_SHARE) * 0.6;
    draw::arc(s, (cx, cy), ring, width, (KNOB_START - KNOB_SWEEP, KNOB_START), t.colour(Tok::Border));
    let a = knob_angle(value);
    if value > 0.0 {
        draw::arc(s, (cx, cy), ring, width, (a, KNOB_START), t.colour(Tok::Primary));
    }
    // The pointer art points at 12 o'clock: turn it the rest of the way.
    let (sn, cs) = sin_cos(a - core::f32::consts::FRAC_PI_2);
    s.turned(&hw.knob_pointer, (cx, cy), sn, cs);
}

/// The fader's size: its track's.
pub fn fader_size(hw: &Hardware) -> (u32, u32) {
    (hw.fader_track.width, hw.fader_track.height)
}

/// Where the fader's cap sits for a value: its centre's y, from the track's top.
pub fn fader_cap_y(hw: &Hardware, value: f32) -> f32 {
    let (top, bottom) = (hw.len(g::FADER_TOP), hw.len(g::FADER_BOTTOM));
    bottom - (bottom - top) * value.clamp(0.0, 1.0)
}

/// A fader at (x, y): the track, and the cap (with its shadow) moved along
/// the slot to the value.
pub fn paint_fader(s: &mut Surface, hw: &Hardware, x: i32, y: i32, value: f32) {
    s.draw(&hw.fader_track, x, y);
    let (cx, cy) = (x as f32 + hw.len(g::FADER_SLOT_X), y as f32 + fader_cap_y(hw, value));
    centred(s, &hw.fader_cap_shadow, cx, cy);
    centred(s, &hw.fader_cap, cx, cy);
}

/// A key filling `r`: its frame 9-sliced around it (the frame holds the
/// shadow, outside `r`). The caller writes the label on top.
pub fn paint_key(s: &mut Surface, hw: &Hardware, r: Rect, look: KeyLook) {
    let i = match look {
        KeyLook::Rest => 0,
        KeyLook::Hover => 1,
        KeyLook::Press(n) => 2 + n.min(5) as usize,
    };
    let (mx, my) = (hw.px(g::KEY_MARGIN_X), hw.px(g::KEY_MARGIN_Y));
    s.nine(&hw.key[i], r.x - mx, r.y - my, r.w + 2 * mx as u32, r.h + 2 * my as u32);
}

/// A toggle key at (x, y), on or off, pressed or not.
pub fn paint_toggle_key(s: &mut Surface, hw: &Hardware, x: i32, y: i32, on: bool, pressed: bool) {
    s.draw(&hw.toggle[on as usize + 2 * pressed as usize], x, y);
}

/// A meter's size for `cells` cells: its bezel's.
pub fn meter_size(hw: &Hardware, cells: u32) -> (u32, u32) {
    let (w, h) = meter_window(hw, cells);
    let edge = 2 * hw.px(g::METER_BEZEL_SLICE) as u32;
    (w + edge, h + edge)
}

/// The bezel's window: the cells, with half a cell's gap all round.
fn meter_window(hw: &Hardware, cells: u32) -> (u32, u32) {
    let pad = hw.len(g::METER_WINDOW_W - g::METER_CELL_W);
    (hw.px(g::METER_WINDOW_W) as u32, (cells as f32 * hw.len(g::METER_CELL_H) + pad + 0.5) as u32)
}

/// A meter at (x, y), its LED ladder lit bottom up for a level in dBFS: lit
/// cells their zone's on tile, the rest their zone's off tile; the bezel
/// round them and the glass over them. The figure beside it is the
/// caller's, so a reader who can't tell the tints still reads the level.
pub fn paint_meter(s: &mut Surface, hw: &Hardware, x: i32, y: i32, cells: u32, db: f32) {
    let (w, h) = meter_size(hw, cells);
    s.nine(&hw.meter_bezel, x, y, w, h);
    let edge = hw.px(g::METER_BEZEL_SLICE);
    let (ww, wh) = meter_window(hw, cells);
    let (wx, wy) = (x + edge, y + edge);
    let pad = hw.len((g::METER_WINDOW_W - g::METER_CELL_W) / 2.0);
    let lit = meter_lit(db, cells);
    let cell_h = hw.len(g::METER_CELL_H);
    for i in 0..cells {
        let level = -60.0 + 60.0 * (i as f32 + 0.5) / cells as f32;
        let zone = match meter_zone(level) {
            Zone::Green => 0,
            Zone::Yellow => 1,
            Zone::Red => 2,
        };
        let tile = &hw.meter_cell[(i < lit) as usize][zone];
        let cy = wy as f32 + wh as f32 - pad - (i + 1) as f32 * cell_h;
        s.draw(tile, (wx as f32 + pad + 0.5) as i32, (cy + 0.5) as i32);
    }
    s.screen(&hw.meter_glass, wx, wy, ww, wh, (g::METER_GLASS_OPACITY * 255.0 + 0.5) as u8);
}

/// Where cell `i` (from the bottom) of a meter at (x, y) has its middle.
pub fn meter_cell_centre(hw: &Hardware, x: i32, y: i32, cells: u32, i: u32) -> (i32, i32) {
    let edge = hw.px(g::METER_BEZEL_SLICE);
    let (ww, wh) = meter_window(hw, cells);
    let pad = hw.len((g::METER_WINDOW_W - g::METER_CELL_W) / 2.0);
    let cy = y as f32 + edge as f32 + wh as f32 - pad - (i as f32 + 0.5) * hw.len(g::METER_CELL_H);
    (x + edge + ww as i32 / 2, cy as i32)
}

/// A lamp at (x, y).
pub fn paint_lamp(s: &mut Surface, hw: &Hardware, x: i32, y: i32, lamp: Lamp) {
    s.draw(&hw.lamp[lamp as usize], x, y);
}

/// The focus ring round a control (2 px of `--color-focus`, 2 px out).
pub fn paint_focus(s: &mut Surface, t: &Theme, r: Rect, radius: f32) {
    draw::focus_ring(s, r, radius, t.colour(Tok::Focus));
}
