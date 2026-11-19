// SPDX-License-Identifier: Apache-2.0
//! THE CRISP OS (TASK-090A): every control of the hardware set, at every
//! scale the OS offers and in both themes, drawn from the set made for that
//! scale with nothing shown larger than its pixels (the crisp guard counts
//! each picture that would be), and every piece the size its scale says.
#![cfg(feature = "ui")]

mod common;

use fimage::blit::Surface;
use fui::controls::paint::{
    knob_size, meter_size, paint_fader, paint_key, paint_knob, paint_lamp, paint_meter, paint_switch, paint_toggle_key,
    switch_size,
};
use fui::hardware::{KeyLook, Lamp, RENDER_SCALE};
use fui::layout::Rect;
use fui::theme::{Mode, Theme};

const SCALES: [u32; 9] = [100, 125, 150, 175, 200, 225, 250, 275, 300];

/// One test, so nothing else in this binary draws while the guard counts.
#[test]
fn every_control_at_every_scale_is_drawn_one_to_one() {
    for theme in ["dark", "light"] {
        let mode = if theme == "light" { Mode::Light } else { Mode::Dark };
        for pct in SCALES {
            let hw = common::build(theme, pct);
            let t = Theme::new(mode, pct as f32 / 100.0);
            // Each piece is the 4x render brought to this scale.
            assert_eq!(hw.knob_body[0].scale as u32 * 25, pct);
            let want = |px4: u32| (px4 * pct).div_ceil(RENDER_SCALE * 100);
            let dark_png =
                std::fs::read(format!("{}/../../assets/hardware/{theme}/lamp_on.png", env!("CARGO_MANIFEST_DIR")))
                    .unwrap();
            let lamp_w4 = u32::from_be_bytes(dark_png[16..20].try_into().unwrap());
            assert_eq!(hw.lamp[1].width, want(lamp_w4), "the lamp at {pct} %");

            let (w, h) = (1200u32, 900u32);
            let mut px = vec![0u32; (w * h) as usize];
            let mut s = Surface { px: &mut px, width: w, height: h, stride: w };
            fimage::guard::expect_scale(pct);
            let _ = fimage::guard::take();
            for on in [false, true] {
                paint_switch(&mut s, &hw, 10, 10, on);
            }
            let (sw, _) = switch_size(&hw);
            for v in [0.0, 0.37, 1.0] {
                paint_knob(&mut s, &hw, &t, 20 + sw as i32, 10, v);
            }
            let k = knob_size(&hw) as i32;
            for v in [0.0, 0.5, 1.0] {
                paint_fader(&mut s, &hw, 40 + sw as i32 + k, 10, v);
            }
            let unit = t.px(40);
            for (i, look) in
                [KeyLook::Rest, KeyLook::Hover, KeyLook::Press(0), KeyLook::Press(5)].into_iter().enumerate()
            {
                paint_key(&mut s, &hw, Rect::new(30 + i as i32 * 3 * unit as i32, 400, 2 * unit, unit), look);
            }
            for (on, pressed) in [(false, false), (true, false), (false, true), (true, true)] {
                paint_toggle_key(&mut s, &hw, 30, 520, on, pressed);
            }
            for cells in [8, 16, 24] {
                let (_, mh) = meter_size(&hw, cells);
                assert!(mh < h, "a meter of {cells} cells fits");
                paint_meter(&mut s, &hw, 700, 10, cells, -6.0);
            }
            for lamp in [Lamp::Off, Lamp::On, Lamp::Warn, Lamp::Stop] {
                paint_lamp(&mut s, &hw, 30, 600, lamp);
            }
            if let Some((n, c)) = fimage::guard::take() {
                panic!("{theme} at {pct} %: {n} picture(s) not crisp, the last {}", fimage::guard::describe("one", &c));
            }
        }
    }
    // The guard itself: the 100 % set on a 150 % screen is caught, piece by
    // piece; a picture with no insets stretched by nine is caught.
    let hw = common::build("dark", 100);
    let mut px = vec![0u32; 400 * 400];
    let mut s = Surface { px: &mut px, width: 400, height: 400, stride: 400 };
    fimage::guard::expect_scale(150);
    let _ = fimage::guard::take();
    paint_lamp(&mut s, &hw, 0, 0, Lamp::On);
    let (n, c) = fimage::guard::take().expect("100 % art on a 150 % screen is caught");
    assert_eq!((n, c.scale as u32 * 25, c.want_pct), (1, 100, 150));
    assert_eq!(fimage::guard::describe("lamp_on", &c), "lamp_on (made for 100 %) on a 150 % screen");
    fimage::guard::expect_scale(100);
    s.nine(&hw.lamp[1], 0, 0, hw.lamp[1].width * 2, hw.lamp[1].height);
    let (_, c) = fimage::guard::take().expect("a picture with no insets stretched is caught");
    assert_eq!((c.dw, c.w, c.want_pct), (hw.lamp[1].width * 2, hw.lamp[1].width, 0));
    fimage::guard::expect_scale(0);
}
