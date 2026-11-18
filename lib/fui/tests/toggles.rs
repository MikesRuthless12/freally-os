// SPDX-License-Identifier: Apache-2.0
//! ⛔ The fleet's toggles under the pointer (owner, 2026-10-23): off is gray,
//! off under the pointer white, on is Freally green whether the pointer is
//! over it or not. The on rule outranks the hover rule. Pinned in both
//! themes for the drawn toggles; the hardware set's switch keeps its own
//! lit state, pinned too.
#![cfg(feature = "ui")]

mod common;

use fimage::blit::Surface;
use fui::controls::paint::{paint_switch, switch_size};
use fui::controls::{Done, Press, Switch, key, toggle_colour};
use fui::theme::{Mode, Theme, Tok};

const THEMES: [Mode; 2] = [Mode::Dark, Mode::Light];

#[test]
fn four_looks_in_both_themes() {
    for m in THEMES {
        let t = Theme::new(m, 1.0);
        assert_eq!(toggle_colour(&t, false, false), t.colour(Tok::Text2), "{m:?}: off is gray");
        assert_eq!(toggle_colour(&t, false, true), t.colour(Tok::Text), "{m:?}: off under the pointer is white");
        assert_eq!(toggle_colour(&t, true, false), t.colour(Tok::PrimaryInk), "{m:?}: on is green");
        assert_eq!(toggle_colour(&t, true, true), t.colour(Tok::PrimaryInk), "{m:?}: on outranks hover");
    }
    let dark = Theme::new(Mode::Dark, 1.0);
    let white = toggle_colour(&dark, false, true);
    assert!(white.r >= 230 && white.g >= 230 && white.b >= 230, "white on the dark ground: {white:?}");
}

#[test]
fn never_the_signal_colour_or_gold() {
    for m in THEMES {
        let t = Theme::new(m, 1.0);
        for (on, hovered) in [(false, false), (false, true), (true, false), (true, true)] {
            let c = toggle_colour(&t, on, hovered);
            for not in [Tok::Warning, Tok::Error, Tok::MeterYellow, Tok::MeterRed] {
                assert_ne!(c, t.colour(not), "{m:?} on={on} hovered={hovered} wears {not:?}");
            }
        }
    }
}

#[test]
fn the_switch_is_the_art_lit_on_and_dark_off() {
    // The hardware set's switch keeps its own lit state; on, its track is
    // Freally green and its knob sits right; off, neither.
    for hw in [common::dark(), common::light()] {
        let t = Theme::new(Mode::Dark, 1.0);
        let (w, h) = switch_size(hw);
        let draw = |on| {
            let mut px = vec![t.colour(Tok::Surface).xrgb(); (w * h) as usize];
            let mut s = Surface { px: &mut px, width: w, height: h, stride: w };
            paint_switch(&mut s, hw, 0, 0, on);
            px
        };
        let (on, off) = (draw(true), draw(false));
        let at = |px: &[u32], x: u32| px[(h / 2 * w + x) as usize];
        // The track's middle, clear of the knob at either end.
        assert!(common::greenish(at(&on, w / 2)), "on: the track is lit: {:06x}", at(&on, w / 2));
        assert!(!common::greenish(at(&off, w / 2)), "off: the track is dark: {:06x}", at(&off, w / 2));
        // The knob's face where it sits: not the track's green.
        let end = hw.px(fui::hardware::geometry::SWITCH_END) as u32;
        assert!(!common::greenish(at(&on, w - end)), "on: the knob sits at the right");
        assert!(common::greenish(at(&on, end)), "on: the left is track: {:06x} (w {w}, end {end})", at(&on, end));
        assert_ne!(at(&off, end), at(&off, w / 2), "off: the knob sits at the left");
    }
}

#[test]
fn space_and_enter_flip_it_both_ways() {
    let mut s = Switch::default();
    assert_eq!(s.press(Press::key(key::SPACE)), Done::Changed);
    assert!(s.on);
    assert_eq!(s.press(Press::key(key::ENTER)), Done::Changed);
    assert!(!s.on);
    assert_eq!(s.press(Press::key(key::LEFT)), Done::No);
    assert!(!s.on);
}
