// SPDX-License-Identifier: Apache-2.0
//! The hardware set as the OS draws it (TASK-056): every piece loads in both
//! themes at each scale, the geometry is `hardware.json`'s, a missing or
//! wrongly scaled piece is refused, and the fader, key and lamp are drawn
//! where they should be.
#![cfg(feature = "ui")]

mod common;

use fimage::blit::Surface;
use fui::controls::paint::{fader_cap_y, fader_size, paint_fader, paint_key, paint_lamp};
use fui::hardware::{Hardware, KeyLook, Lamp, PIECES, RENDER_SCALE, geometry as g, slice_of};
use fui::layout::Rect;

fn json(theme: &str) -> fjson::Value {
    let p = format!("{}/../../assets/hardware/{theme}/hardware.json", env!("CARGO_MANIFEST_DIR"));
    fjson::parse(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn num(v: &fjson::Value, path: &[&str]) -> f32 {
    let mut at = v;
    for k in path {
        at = at.get(k).unwrap_or_else(|| panic!("hardware.json: no {path:?}"));
    }
    at.as_f64().unwrap() as f32
}

#[test]
fn the_geometry_is_hardware_json_s() {
    for theme in ["dark", "light"] {
        let j = json(theme);
        let pairs = [
            (g::SWITCH_END, num(&j, &["switch", "endPx"])),
            (g::KNOB_FRAMES as f32, num(&j, &["knob", "frames"])),
            (g::KNOB_PERIOD_DEG, num(&j, &["knob", "periodDeg"])),
            (g::KNOB_STEP_DEG, num(&j, &["knob", "stepDeg"])),
            (g::KNOB_BODY_SHARE, num(&j, &["knob", "bodyShare"])),
            (g::FADER_SLOT_X, num(&j, &["fader", "slotX"])),
            (g::FADER_TOP, num(&j, &["fader", "travel", "top"])),
            (g::FADER_BOTTOM, num(&j, &["fader", "travel", "bottom"])),
            (g::KEY_MARGIN_X, (num(&j, &["button", "frameW"]) - num(&j, &["button", "key", "w"])) / 2.0),
            (g::KEY_MARGIN_Y, (num(&j, &["button", "frameH"]) - num(&j, &["button", "key", "h"])) / 2.0),
            (g::METER_BEZEL_SLICE, num(&j, &["meter", "bezel", "slice"])),
            (g::METER_WINDOW_W, num(&j, &["meter", "bezel", "window", "w"])),
            // The upright tile is the across tile turned: its width is px.h.
            (g::METER_CELL_W, num(&j, &["meter", "cell", "px", "h"])),
            (g::METER_CELL_H, num(&j, &["meter", "cell", "px", "w"])),
            (g::METER_GLASS_OPACITY, num(&j, &["meter", "glass", "opacity"])),
            (64.0, num(&j, &["button", "slice", "inline"])),
            (40.0, num(&j, &["button", "slice", "block"])),
            (RENDER_SCALE as f32, num(&j, &["meter", "cell", "scale"])),
        ];
        for (i, (ours, theirs)) in pairs.iter().enumerate() {
            assert!((ours - theirs).abs() < 1e-3, "{theme}: pair {i}: {ours} here, {theirs} in hardware.json");
        }
        assert_eq!(slice_of("frames/button_rest"), [64, 40, 64, 40]);
        assert_eq!(slice_of("meter_bezel"), [44; 4]);
    }
}

#[test]
fn every_piece_loads_in_both_themes_at_each_scale() {
    for hw in [common::dark(), common::light(), common::dark_2x()] {
        assert_eq!(hw.knob_body.len(), 16);
        assert_eq!(hw.key.len(), 8);
        assert_eq!(hw.toggle.len(), 4);
    }
    // At 2x every piece is twice its 1x size (within the downscale's rounding).
    let (one, two) = (common::dark(), common::dark_2x());
    assert!((two.fader_track.width as i32 - 2 * one.fader_track.width as i32).abs() <= 1);
    assert_eq!(two.knob_body[0].width, 2 * one.knob_body[0].width);
    assert_eq!(PIECES.len(), 49);
}

#[test]
fn a_missing_or_wrongly_scaled_piece_is_refused() {
    let png = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/hardware/dark/lamp_on.png")).unwrap();
    let at_2x: &'static [u8] =
        Box::leak(fimage::fimg::from_png(&png, 200, RENDER_SCALE, [0; 4]).unwrap().into_boxed_slice());
    let err = Hardware::load(100, |_| Some(at_2x)).unwrap_err();
    assert!(err.contains("for 200 %, not 100 %"), "{err}");
    let err = Hardware::load(200, |p| if p == "fader_cap" { None } else { Some(at_2x) }).unwrap_err();
    assert!(err.contains("no fader_cap"), "{err}");
    let err = Hardware::load(100, |_| Some(&b"FIMG nonsense"[..])).unwrap_err();
    assert!(err.starts_with("hardware: switch_track_off:"), "{err}");
}

#[test]
fn the_fader_cap_moves_along_its_slot() {
    let hw = common::dark();
    let (top, bottom) = (fader_cap_y(hw, 1.0), fader_cap_y(hw, 0.0));
    assert!(
        (top - 31.5).abs() < 1e-3 && (bottom - 181.5).abs() < 1e-3,
        "{top}..{bottom}: hardware.json's travel at 1x"
    );
    assert!((fader_cap_y(hw, 0.5) - 106.5).abs() < 1e-3);
    let (w, h) = fader_size(hw);
    let draw = |v| {
        let mut px = vec![0u32; (w * h) as usize];
        let mut s = Surface { px: &mut px, width: w, height: h, stride: w };
        paint_fader(&mut s, hw, 0, 0, v);
        px
    };
    let (up, down) = (draw(1.0), draw(0.0));
    let slot = hw.px(g::FADER_SLOT_X) as u32;
    let at = |px: &[u32], y: f32| px[(y as u32 * w + slot) as usize];
    // The cap is lighter than the bare slot where it sits.
    assert!(common::luma(at(&up, top)) > common::luma(at(&down, top)) + 20, "the cap at the top at 1");
    assert!(common::luma(at(&down, bottom)) > common::luma(at(&up, bottom)) + 20, "and at the bottom at 0");
}

#[test]
fn the_key_s_frame_wraps_its_rect_and_presses_down() {
    let hw = common::dark();
    let r = Rect::new(20, 20, 80, 25);
    let draw = |look| {
        let mut px = vec![0u32; 120 * 65];
        let mut s = Surface { px: &mut px, width: 120, height: 65, stride: 120 };
        paint_key(&mut s, hw, r, look);
        px
    };
    let rest = draw(KeyLook::Rest);
    let face = rest[(20 + 12) * 120 + 60];
    assert!(common::luma(face) > 10, "the key's face is drawn: {face:06x}");
    assert_eq!(rest[0], 0, "nothing past the frame's margin");
    assert_ne!(draw(KeyLook::Press(5)), rest, "pressed looks pressed");
    assert_ne!(draw(KeyLook::Hover), rest, "hovered looks hovered");
    assert_eq!(draw(KeyLook::Press(9)), draw(KeyLook::Press(5)), "held at the last frame");
}

#[test]
fn lamps_light_in_their_colours() {
    let hw = common::dark();
    let n = hw.lamp[0].width;
    let draw = |lamp| {
        let mut px = vec![0u32; (n * n) as usize];
        let mut s = Surface { px: &mut px, width: n, height: n, stride: n };
        paint_lamp(&mut s, hw, 0, 0, lamp);
        px[(n / 2 * n + n / 2) as usize]
    };
    assert!(common::greenish(draw(Lamp::On)), "on: green");
    let (r, g, _) = {
        let p = draw(Lamp::Stop);
        (p >> 16 & 0xFF, p >> 8 & 0xFF, p & 0xFF)
    };
    assert!(r > g + 60, "stop: red");
    assert!(common::luma(draw(Lamp::Off)) + 30 < common::luma(draw(Lamp::On)), "off: dark");
}
