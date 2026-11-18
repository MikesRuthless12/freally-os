// SPDX-License-Identifier: Apache-2.0
//! The fleet's knob (TASK-056): the art's body, turned frame by frame with
//! the value; its sweep from 7:30 to 4:30; the lit arc in Freally green from
//! the start to the pointer and the dim track beyond, in the ring round the
//! body; the art's green pointer turned to the value; the keys that turn it.
#![cfg(feature = "ui")]

mod common;

use core::f32::consts::PI;
use fimage::blit::Surface;
use fui::controls::paint::{knob_frame, knob_size, paint_knob};
use fui::controls::{Done, Press, Ranged, key, knob_angle};
use fui::hardware::Hardware;
use fui::theme::{Mode, Theme, Tok};

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-5
}

#[test]
fn it_sweeps_from_half_past_seven_to_half_past_four() {
    assert!(close(knob_angle(0.0), 1.25 * PI), "7:30 at 0");
    assert!(close(knob_angle(0.5), 0.5 * PI), "12 o'clock at half");
    assert!(close(knob_angle(1.0), -0.25 * PI), "4:30 at 1");
    assert!(close(knob_angle(-3.0), knob_angle(0.0)) && close(knob_angle(9.0), knob_angle(1.0)), "held at its ends");
}

#[test]
fn the_body_turns_through_its_frames() {
    // 270° of travel; the knurling repeats every 9°, 16 frames to a period.
    assert_eq!(knob_frame(0.0), 0);
    assert_eq!(knob_frame(4.5 / 270.0), 8, "half a period on");
    assert_eq!(knob_frame(9.0 / 270.0 + 1e-4), 0, "a whole period: the same frame");
    assert_eq!(knob_frame(1.0), 0, "270° is 30 periods");
}

fn knob(hw: &Hardware, m: Mode, value: f32) -> (Vec<u32>, Theme, u32) {
    let t = Theme::new(m, 1.0);
    let n = knob_size(hw);
    let mut px = vec![t.colour(Tok::Surface).xrgb(); (n * n) as usize];
    let mut s = Surface { px: &mut px, width: n, height: n, stride: n };
    paint_knob(&mut s, hw, &t, 0, 0, value);
    (px, t, n)
}

#[test]
fn the_arc_is_lit_green_from_the_start_to_the_pointer() {
    for (hw, m) in [(common::dark(), Mode::Dark), (common::light(), Mode::Light)] {
        let (half, t, n) = knob(hw, m, 0.5);
        // On the ring, the middle of its width: 0.9 of the way out.
        let ring = (n as f32 / 2.0 * 0.9) as u32;
        let (nine, three) = (((n / 2) * n + n / 2 - ring) as usize, ((n / 2) * n + n / 2 + ring) as usize);
        let green = t.colour(Tok::Primary).xrgb();
        assert_eq!(half[nine], green, "{m:?}: 9 o'clock is between the start and the pointer: lit");
        assert_eq!(half[three], t.colour(Tok::Border).xrgb(), "{m:?}: 3 o'clock is past the pointer: the dim track");
        let (zero, ..) = knob(hw, m, 0.0);
        assert_eq!(zero[nine], t.colour(Tok::Border).xrgb(), "{m:?}: nothing lit at 0");
        let (full, ..) = knob(hw, m, 1.0);
        assert_eq!(full[three], green, "{m:?}: all lit at 1");
    }
}

#[test]
fn the_pointer_is_the_art_s_green_turned_to_the_value() {
    let hw = common::dark();
    let (half, _, n) = knob(hw, Mode::Dark, 0.5);
    let c = n as f32 / 2.0;
    // The art's bar runs from about 0.22 to 0.70 of the way out; read 0.45.
    let out = c * 0.45;
    let at = |px: &[u32], a: f32| {
        let (x, y) = (c + a.cos() * out, c - a.sin() * out);
        px[y as usize * n as usize + x as usize]
    };
    assert!(common::greenish(at(&half, 0.5 * PI)), "at half it points up: {:06x}", at(&half, 0.5 * PI));
    assert!(!common::greenish(at(&half, 1.25 * PI)), "and not toward 7:30");
    let (zero, ..) = knob(hw, Mode::Dark, 0.0);
    assert!(common::greenish(at(&zero, 1.25 * PI)), "at 0 it points to 7:30: {:06x}", at(&zero, 1.25 * PI));
    assert!(!common::greenish(at(&zero, 0.5 * PI)), "and not up");
}

#[test]
fn the_keys_turn_it_in_steps_and_stop_at_the_ends() {
    let mut r = Ranged::new(0.5, 0.01);
    assert_eq!(r.press(Press::key(key::UP)), Done::Changed);
    assert!(close(r.value, 0.51));
    r.press(Press::key(key::PAGE_DOWN));
    assert!(close(r.value, 0.41));
    assert_eq!(r.press(Press::key(key::END)), Done::Changed);
    assert_eq!(r.value, 1.0);
    assert_eq!(r.press(Press::key(key::RIGHT)), Done::Moved, "already at the top");
    r.press(Press::key(key::HOME));
    assert_eq!(r.value, 0.0);
    assert_eq!(r.press(Press::key(key::ENTER)), Done::No);
}
