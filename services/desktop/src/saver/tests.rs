// SPDX-License-Identifier: Apache-2.0
//! The savers on the host, drawn with the real F (`assets/os/dark/saver_f.png`).

extern crate std;

use super::*;
use alloc::vec;

fn the_f() -> Sprite {
    let path = std::format!("{}/../../assets/os/dark/saver_f.png", env!("CARGO_MANIFEST_DIR"));
    let png = std::fs::read(&path).unwrap();
    let d = fimage::png::decode(&png).unwrap();
    assert_eq!(d.channels, 4);
    // PNG is straight RGBA; the savers take premultiplied BGRA.
    let px = d
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&[r, g, b, a]| {
            let m = |c: u8| (c as u32 * a as u32 / 255) as u8;
            [m(b), m(g), m(r), a]
        })
        .collect();
    Sprite { w: d.w, h: d.h, px }
}

const W: u32 = 640;
const H: u32 = 400;

/// Pixels clearly Freally green.
fn greens(px: &[u32]) -> usize {
    px.iter()
        .filter(|&&p| {
            let (r, g, b) = ((p >> 16 & 0xFF) as i32, (p >> 8 & 0xFF) as i32, (p & 0xFF) as i32);
            g > 90 && g > r + 30 && g > b + 30
        })
        .count()
}

fn run(s: &mut dyn Saver, px: &mut [u32], frames: u32, dt: u32) {
    for _ in 0..frames {
        s.frame(&mut Frame { px, w: W, h: H }, dt);
    }
}

#[test]
fn bounce_glides_leaves_a_glow_and_bursts_in_a_corner() {
    let f = the_f();
    let mut b = bounce::Bounce::new(W, H, &f, 7);
    let mut px = vec![0u32; (W * H) as usize];
    run(&mut b, &mut px, 1, 33);
    let first = px.clone();
    assert!(greens(&px) > 2000, "the F shows: {} green pixels", greens(&px));
    run(&mut b, &mut px, 10, 33);
    assert_ne!(px, first, "it moves");
    // Aimed into the top-left corner along its diagonal: one burst.
    b.aim(60.0, 60.0 * (H as f32 - 100.0) / (W as f32 - 160.0));
    run(&mut b, &mut px, 60, 33);
    assert_eq!(b.corners, 1);
}

#[test]
fn the_block_turns_showing_one_to_three_faces() {
    let f = the_f();
    let mut b = block::Block::new(W, H, &f);
    let mut px = vec![0u32; (W * H) as usize];
    let mut seen = std::collections::BTreeSet::new();
    let mut last = Vec::new();
    for i in 0..120 {
        run(&mut b, &mut px, 1, 100);
        assert!((1..=3).contains(&b.faces_drawn), "frame {i}: {} faces", b.faces_drawn);
        seen.insert(b.faces_drawn);
        assert!(greens(&px) > 300, "frame {i}: the edges and the F are green");
        assert_ne!(px, last, "frame {i}: it turns");
        last = px.clone();
    }
    assert!(seen.len() >= 2, "seen from more than one side: {seen:?}");
    // Nothing outside the block: the corners stay black.
    assert_eq!(px[0], 0);
    assert_eq!(px[(W * H - 1) as usize], 0);
}

#[test]
fn the_swarm_forms_the_f_and_bursts() {
    let f = the_f();
    let mut s = swarm::Swarm::new(W, H, &f, 3);
    let mut px = vec![0u32; (W * H) as usize];
    // Gathered by the end of its 5 s, and held.
    run(&mut s, &mut px, 180, 33);
    assert_eq!(s.phase(), swarm::Phase::Hold);
    let placed = s.in_place(6.0);
    assert!(placed * 10 >= swarm::SPARKS * 9, "{placed} of {} sparks in the F", swarm::SPARKS);
    // Thrown out at 8 s: scattered a quarter of a second later.
    run(&mut s, &mut px, 70, 33);
    assert_eq!(s.phase(), swarm::Phase::Burst);
    assert!(s.in_place(6.0) * 10 < swarm::SPARKS, "{} still in place after the burst", s.in_place(6.0));
}

#[test]
fn warp_flies_toward_the_f() {
    let f = the_f();
    let mut wp = warp::Warp::new(W, H, &f, 5);
    let mut px = vec![0u32; (W * H) as usize];
    run(&mut wp, &mut px, 30, 33);
    assert!(greens(&px) > 3000, "{} green pixels", greens(&px));
    // The big F waits in the middle.
    let mid = &px[(H / 2 * W) as usize..((H / 2 + 1) * W) as usize];
    assert!(greens(&mid[(W / 2 - 40) as usize..(W / 2 + 40) as usize]) > 10);
}

#[test]
fn reduce_motion_is_the_f_still_and_dimmed() {
    let f = the_f();
    let mut s = still::Still::new(H, &f);
    let mut px = vec![0x00AB_CDEFu32; (W * H) as usize];
    run(&mut s, &mut px, 1, 33);
    let once = px.clone();
    run(&mut s, &mut px, 5, 33);
    assert_eq!(px, once, "nothing moves");
    // Nothing that was behind shows; the F is there, and no brighter than DIM allows.
    assert!(!px.contains(&0x00AB_CDEF));
    assert!(greens(&px) > 500);
    let brightest = px.iter().map(|&p| p >> 8 & 0xFF).max().unwrap();
    assert!(brightest <= still::DIM + 1, "{brightest}");
}

/// Frames a second at 2560×1600, per saver, on this machine (`cargo test
/// --release -p svc-desktop -- --ignored --nocapture saver_fps`).
#[test]
#[ignore]
fn saver_fps() {
    let f = the_f();
    let (w, h) = (2560u32, 1600u32);
    for kind in Kind::ALL {
        let mut s = new(kind, w, h, &f, 1);
        let mut px = vec![0u32; (w * h) as usize];
        let t = std::time::Instant::now();
        for _ in 0..60 {
            s.frame(&mut Frame { px: &mut px, w, h }, 33);
        }
        let fps = 60.0 / t.elapsed().as_secs_f64();
        std::println!("{}: {fps:.0} frames a second at {w}x{h}", kind.name());
    }
}
