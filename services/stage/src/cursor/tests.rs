// SPDX-License-Identifier: Apache-2.0
//! The fleet cursor's motion, worked out from the cursor kit's numbers.

use super::*;
use fimage::blit::Surface;
use fui::color::Rgba;

const MS: u64 = 1_000_000;

fn close(a: f32, b: f32, eps: f32) -> bool {
    (a - b).abs() <= eps
}

#[test]
fn exp_without_libm() {
    assert!(close(exp_neg(0.0), 1.0, 1e-7));
    assert!(close(exp_neg(1.0), 0.367_879_44, 1e-6));
    assert!(close(exp_neg(0.5), 0.606_530_66, 1e-6));
    assert!(close(exp_neg(10.0), 4.539_993e-5, 1e-9));
    assert_eq!(exp_neg(100.0), 0.0);
}

#[test]
fn the_dot_stays_inside_the_ring() {
    assert!(close(reach(1.0), 12.5, 1e-6), "17 − 3 − 1.5");
    let mut c = Cursor::new(500, 400, 0);
    c.move_to(600, 400);
    assert!(c.tick(MS));
    assert!(close(c.ring.0, 600.0 - 12.5, 0.01), "held at its reach: {:?}", c.ring);
}

#[test]
fn moving_down_left_the_dot_rides_in_the_lower_left() {
    let mut c = Cursor::new(500, 400, 0);
    c.move_to(470, 430);
    c.tick(16 * MS);
    assert!(c.dot.0 < c.ring.0 && c.dot.1 > c.ring.1, "dot {:?} ring {:?}", c.dot, c.ring);
}

#[test]
fn the_ring_catches_up_and_then_costs_nothing() {
    let mut c = Cursor::new(500, 400, 0);
    c.move_to(520, 400);
    let mut t = 0;
    while c.tick(t) {
        t += 16 * MS;
        assert!(t < 2_000 * MS, "still moving after 2 s");
    }
    assert_eq!(c.ring, c.dot);
    // At rest: no frames.
    assert!(!c.tick(t + 16 * MS));
    assert!(!c.moving(t + 32 * MS));
}

#[test]
fn the_trail_is_time_based_70_ms() {
    // Halfway (from inside the reach) takes 70 ms × ln 2 ≈ 48.5 ms.
    let mut c = Cursor::new(500, 400, 0);
    c.move_to(510, 400);
    c.tick(485 * MS / 10);
    assert!(close(c.ring.0, 505.0, 0.05), "{:?}", c.ring);
}

#[test]
fn hovering_grows_and_pressing_shrinks_for_at_least_140_ms() {
    let mut c = Cursor::new(100, 100, 0);
    c.hover = true;
    assert_eq!(c.target(0), HOVER);
    let mut t = 0;
    while c.tick(t) {
        t += 16 * MS;
    }
    assert_eq!(c.scale, HOVER);
    c.press(true, t);
    assert_eq!(c.target(t), HOVER * PRESS);
    c.tick(t + 10 * MS);
    c.press(false, t + 10 * MS);
    assert_eq!(c.target(t + 100 * MS), HOVER * PRESS, "a quick click still shows for 140 ms");
    assert!(c.tick(t + 100 * MS));
    assert_eq!(c.target(t + 150 * MS), HOVER, "then it comes back");
    let mut u = t + 150 * MS;
    while c.tick(u) {
        u += 16 * MS;
    }
    assert_eq!(c.scale, HOVER);
}

#[test]
fn reduce_motion_drops_the_trail_and_the_easing() {
    let mut c = Cursor::new(500, 400, 0);
    c.reduce_motion = true;
    c.move_to(300, 200);
    c.hover = true;
    assert!(!c.tick(MS));
    assert_eq!((c.ring, c.scale), (c.dot, HOVER));
}

#[test]
fn its_bounds_hold_the_ring_and_the_dot() {
    let mut c = Cursor::new(500, 400, 0);
    c.move_to(530, 420);
    c.tick(5 * MS);
    let b = c.bounds();
    for (x, y) in [c.dot, c.ring] {
        let r = RING * HOVER / 2.0;
        assert!(b.contains((x - r) as i32, (y - r) as i32) && b.contains((x + r) as i32, (y + r) as i32), "{b:?}");
    }
}

#[test]
fn only_the_arrow_and_the_hand_are_the_ring() {
    assert!(Kind::Arrow.is_ring() && Kind::Hand.is_ring());
    assert!(!Kind::Text.is_ring() && !Kind::ResizeEw.is_ring() && !Kind::NotAllowed.is_ring());
    assert_eq!(Kind::from_u8(2), Some(Kind::Text));
    assert_eq!(Kind::from_u8(11), None);
}

#[test]
fn the_hover_fill_is_mixed_in_srgb_as_the_kit_mixes_it() {
    let ink = Rgba::new(121, 217, 32, 255);
    let mut px = alloc::vec![0x090C09u32; 64 * 64];
    let mut s = Surface { px: &mut px, width: 64, height: 64, stride: 64 };
    fill_srgb(&mut s, (32.0, 32.0), 20.0, ink, FILL);
    // 14 % of the way from (9, 12, 9) to the ink, in sRGB.
    assert_eq!(px[32 * 64 + 32], 0x19290C);
    assert_eq!(px[0], 0x090C09, "outside it");
}
