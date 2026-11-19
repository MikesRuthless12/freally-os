// SPDX-License-Identifier: Apache-2.0
//! The playbook's hardware set, as the controls draw it (TASK-056, FR-025):
//! the switch, the knob, the fader, the key, the toggle key, the meter and
//! the lamps, each a FIMG at the UI's scale (1x, 2x or 3x), never rendered
//! or retouched here. The art is layers the controls light or move; the
//! values stay the engine's.
//!
//! The geometry is `assets/hardware/<theme>/hardware.json`'s, at the
//! renders' 4x; `len` brings a length to this set's scale. A test holds the
//! numbers here to the file.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use fimage::fimg::{self, Fimg};

/// The renders' density.
pub const RENDER_SCALE: u32 = 4;

/// Every piece the OS draws, by its path under the theme's folder (no
/// extension). The image carries exactly these, at each scale.
pub const PIECES: &[&str] = &[
    "switch_track_off",
    "switch_track_on",
    "switch_knob",
    "switch_knob_shadow",
    "frames/knob_00",
    "frames/knob_01",
    "frames/knob_02",
    "frames/knob_03",
    "frames/knob_04",
    "frames/knob_05",
    "frames/knob_06",
    "frames/knob_07",
    "frames/knob_08",
    "frames/knob_09",
    "frames/knob_10",
    "frames/knob_11",
    "frames/knob_12",
    "frames/knob_13",
    "frames/knob_14",
    "frames/knob_15",
    "knob_pointer",
    "knob_shadow",
    "fader_track",
    "fader_cap",
    "fader_cap_shadow",
    "frames/button_rest",
    "frames/button_hover",
    "frames/button_press0",
    "frames/button_press1",
    "frames/button_press2",
    "frames/button_press3",
    "frames/button_press4",
    "frames/button_press5",
    "frames/toggle_off",
    "frames/toggle_on",
    "frames/toggle_offPressed",
    "frames/toggle_onPressed",
    "meter_cell_off_green",
    "meter_cell_off_yellow",
    "meter_cell_off_red",
    "meter_cell_on_green",
    "meter_cell_on_yellow",
    "meter_cell_on_red",
    "meter_bezel",
    "meter_glass",
    "lamp_off",
    "lamp_on",
    "lamp_warn",
    "lamp_stop",
];

/// A piece's 9-slice insets (left, top, right, bottom) at the renders' 4x,
/// from `hardware.json`: the meter's bezel (`slice` 44) and the key's
/// frames (`slice.inline` 64, `slice.block` 40). `piece` is its path, as in
/// `PIECES`.
pub fn slice_of(piece: &str) -> [u32; 4] {
    if piece == "meter_bezel" {
        [44; 4]
    } else if piece.starts_with("frames/button_") {
        [64, 40, 64, 40]
    } else {
        [0; 4]
    }
}

/// `hardware.json`'s geometry, at 4x.
pub mod geometry {
    /// The switch knob's centre, in from the track's end.
    pub const SWITCH_END: f32 = 73.6;
    /// The knob's frames: 16 over a 9° period of its knurling, clockwise.
    pub const KNOB_FRAMES: usize = 16;
    pub const KNOB_PERIOD_DEG: f32 = 9.0;
    pub const KNOB_STEP_DEG: f32 = 0.5625;
    /// The body's share of the frame; the lit arc runs in the rest.
    pub const KNOB_BODY_SHARE: f32 = 0.8;
    /// The fader's slot, and the cap's travel along it.
    pub const FADER_SLOT_X: f32 = 183.0;
    pub const FADER_TOP: f32 = 126.0;
    pub const FADER_BOTTOM: f32 = 726.0;
    /// The key's frame around the key itself.
    pub const KEY_MARGIN_X: f32 = 40.0;
    pub const KEY_MARGIN_Y: f32 = 40.0;
    /// The meter: its bezel's window and the cells inside it.
    pub const METER_BEZEL_SLICE: f32 = 44.0;
    pub const METER_WINDOW_W: f32 = 56.0;
    pub const METER_CELL_W: f32 = 48.0;
    pub const METER_CELL_H: f32 = 24.0;
    pub const METER_GLASS_OPACITY: f32 = 0.4;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lamp {
    Off,
    On,
    Warn,
    Stop,
}

/// The key's looks: at rest, under the pointer, and six frames of its press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyLook {
    Rest,
    Hover,
    /// 0 (just touched) to 5 (all the way down).
    Press(u8),
}

#[derive(Debug)]
pub struct Hardware<'a> {
    /// The UI scale it was made for, in percent (100–300).
    pub scale: u32,
    /// Off, on.
    pub switch_track: [Fimg<'a>; 2],
    pub switch_knob: Fimg<'a>,
    pub switch_knob_shadow: Fimg<'a>,
    pub knob_body: Vec<Fimg<'a>>,
    pub knob_pointer: Fimg<'a>,
    pub knob_shadow: Fimg<'a>,
    pub fader_track: Fimg<'a>,
    pub fader_cap: Fimg<'a>,
    pub fader_cap_shadow: Fimg<'a>,
    /// Rest, hover, press 0–5.
    pub key: Vec<Fimg<'a>>,
    /// Off, on, off pressed, on pressed.
    pub toggle: Vec<Fimg<'a>>,
    /// [off, on][green, yellow, red].
    pub meter_cell: [[Fimg<'a>; 3]; 2],
    pub meter_bezel: Fimg<'a>,
    pub meter_glass: Fimg<'a>,
    /// Off, on, warn, stop.
    pub lamp: [Fimg<'a>; 4],
}

impl<'a> Hardware<'a> {
    /// The set at `scale` percent (TASK-090A: one set for each scale the OS
    /// offers, drawn 1:1), each piece's FIMG from `get` (given a piece's
    /// path from `PIECES`). A missing, damaged or wrongly scaled piece is an
    /// error naming it.
    pub fn load(scale: u32, mut get: impl FnMut(&str) -> Option<&'a [u8]>) -> Result<Hardware<'a>, String> {
        let mut one = |name: &str| -> Result<Fimg<'a>, String> {
            let bytes = get(name).ok_or_else(|| format!("hardware: no {name}"))?;
            let f = fimg::parse(bytes).map_err(|e| format!("hardware: {name}: {e}"))?;
            if f.scale as u32 * 25 != scale {
                return Err(format!("hardware: {name} is for {} %, not {scale} %", f.scale as u32 * 25));
            }
            Ok(f)
        };
        let many = |one: &mut dyn FnMut(&str) -> Result<Fimg<'a>, String>, names: &[&str]| {
            names.iter().map(|n| one(n)).collect::<Result<Vec<_>, _>>()
        };
        let knob_names: Vec<String> = (0..geometry::KNOB_FRAMES).map(|i| format!("frames/knob_{i:02}")).collect();
        let knob_refs: Vec<&str> = knob_names.iter().map(String::as_str).collect();
        Ok(Hardware {
            scale,
            switch_track: [one("switch_track_off")?, one("switch_track_on")?],
            switch_knob: one("switch_knob")?,
            switch_knob_shadow: one("switch_knob_shadow")?,
            knob_body: many(&mut one, &knob_refs)?,
            knob_pointer: one("knob_pointer")?,
            knob_shadow: one("knob_shadow")?,
            fader_track: one("fader_track")?,
            fader_cap: one("fader_cap")?,
            fader_cap_shadow: one("fader_cap_shadow")?,
            key: many(
                &mut one,
                &[
                    "frames/button_rest",
                    "frames/button_hover",
                    "frames/button_press0",
                    "frames/button_press1",
                    "frames/button_press2",
                    "frames/button_press3",
                    "frames/button_press4",
                    "frames/button_press5",
                ],
            )?,
            toggle: many(
                &mut one,
                &["frames/toggle_off", "frames/toggle_on", "frames/toggle_offPressed", "frames/toggle_onPressed"],
            )?,
            meter_cell: [
                [one("meter_cell_off_green")?, one("meter_cell_off_yellow")?, one("meter_cell_off_red")?],
                [one("meter_cell_on_green")?, one("meter_cell_on_yellow")?, one("meter_cell_on_red")?],
            ],
            meter_bezel: one("meter_bezel")?,
            meter_glass: one("meter_glass")?,
            lamp: [one("lamp_off")?, one("lamp_on")?, one("lamp_warn")?, one("lamp_stop")?],
        })
    }

    /// A length from `hardware.json` (4x) at this set's scale.
    pub fn len(&self, v: f32) -> f32 {
        v * self.scale as f32 / (RENDER_SCALE * 100) as f32
    }

    /// The same, in whole pixels.
    pub fn px(&self, v: f32) -> i32 {
        (self.len(v) + 0.5) as i32
    }
}
