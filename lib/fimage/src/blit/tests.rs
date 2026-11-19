// SPDX-License-Identifier: Apache-2.0
//! Golden tests: compositing, clipping, 9-slice, FIMG and the wallpaper's
//! cover, each pixel worked out by hand.

extern crate std;

use super::*;
use crate::fimg::{self, Fimg};
use alloc::vec;
use alloc::vec::Vec;

fn img(w: u32, h: u32, slice: [u16; 4], px: &[u8]) -> Fimg<'_> {
    Fimg { width: w, height: h, scale: 4, slice, pixels: px }
}

#[test]
fn over_blends_premultiplied() {
    // Opaque replaces; transparent keeps; half-transparent red on blue.
    assert_eq!(over(0x0000FF, [0, 255, 0, 255]), 0x00FF00);
    assert_eq!(over(0x123456, [0, 0, 0, 0]), 0x123456);
    // Red at 50% premultiplied is (0, 0, 128, 128) in BGRA.
    assert_eq!(over(0x0000FF, [0, 0, 128, 128]), 0x80007F);
}

#[test]
fn draw_clips_to_the_surface() {
    let px = [0u8, 255, 0, 255].repeat(4); // 2×2 opaque green
    let src = img(2, 2, [0; 4], &px);
    let mut buf = vec![0u32; 9];
    let mut s = Surface { px: &mut buf, width: 3, height: 3, stride: 3 };
    s.draw(&src, 2, -1); // only the bottom-left of the image lands, at (2, 0)
    assert_eq!(buf, [0, 0, 0x00FF00, 0, 0, 0, 0, 0, 0]);
}

#[test]
fn nine_keeps_corners_and_stretches_the_rest() {
    // 3×3 source, insets 1: each cell its own colour.
    let colours: [[u8; 4]; 9] = core::array::from_fn(|i| [i as u8 * 10, 0, 0, 255]);
    let px: Vec<u8> = colours.iter().flatten().copied().collect();
    let src = img(3, 3, [1, 1, 1, 1], &px);
    let mut buf = vec![0u32; 5 * 4];
    let mut s = Surface { px: &mut buf, width: 5, height: 4, stride: 5 };
    s.nine(&src, 0, 0, 5, 4);
    let at = |x: usize, y: usize| buf[y * 5 + x] & 0xFF;
    // Corners as drawn.
    assert_eq!([at(0, 0), at(4, 0), at(0, 3), at(4, 3)], [0, 20, 60, 80]);
    // The top edge stretched across, the left edge down, the middle both.
    assert_eq!([at(1, 0), at(2, 0), at(3, 0)], [10, 10, 10]);
    assert_eq!([at(0, 1), at(0, 2)], [30, 30]);
    assert!((1..4).all(|x| (1..3).all(|y| at(x, y) == 40)));
}

#[test]
fn fimg_round_trips_and_refuses_damage() {
    let px = vec![1u8; 2 * 3 * 4];
    let bytes = fimg::encode(2, 3, 8, [0, 1, 0, 1], &px);
    assert_eq!(bytes.len(), fimg::HEADER_LEN + px.len());
    let f = fimg::parse(&bytes).unwrap();
    assert_eq!((f.width, f.height, f.scale, f.slice), (2, 3, 8, [0, 1, 0, 1]));
    assert_eq!(f.pixels, &px[..]);
    assert!(fimg::parse(&bytes[..bytes.len() - 1]).is_err());
    let mut bad = bytes.clone();
    bad[16] = 13; // 325 %: a scale the OS doesn't offer
    assert!(fimg::parse(&bad).is_err());
    bad[16] = 3; // version 1's 3x: under 100 %
    assert!(fimg::parse(&bad).is_err());
    bad[16] = 6; // 150 %
    assert_eq!(fimg::parse(&bad).unwrap().scale, 6);
    let wide = fimg::encode(2, 3, 4, [2, 0, 1, 0], &px); // insets 3 > width 2
    assert!(fimg::parse(&wide).is_err());
}

#[test]
fn rgba_becomes_premultiplied_bgra() {
    assert_eq!(fimg::from_rgba(&[200, 100, 50, 255]), [50, 100, 200, 255]);
    assert_eq!(fimg::from_rgba(&[200, 100, 50, 0]), [0, 0, 0, 0]);
    assert_eq!(fimg::from_rgba(&[255, 255, 255, 128]), [128, 128, 128, 128]);
}

#[test]
fn turned_quarter_turns_exactly() {
    // A B / C D, a quarter turn counter-clockwise: B D / A C.
    let px = [[1u8, 0, 0, 255], [2, 0, 0, 255], [3, 0, 0, 255], [4, 0, 0, 255]].concat();
    let src = img(2, 2, [0; 4], &px);
    let mut buf = vec![0u32; 4];
    let mut s = Surface { px: &mut buf, width: 2, height: 2, stride: 2 };
    s.turned(&src, (1.0, 1.0), 0.0, 1.0);
    assert_eq!(buf, vec![1, 2, 3, 4], "no turn: drawn as it is");
    let mut buf = vec![0u32; 4];
    let mut s = Surface { px: &mut buf, width: 2, height: 2, stride: 2 };
    s.turned(&src, (1.0, 1.0), 1.0, 0.0);
    assert_eq!(buf, vec![2, 4, 1, 3]);
}

#[test]
fn turned_samples_between_pixels_and_fades_at_its_edge() {
    let px = [0u8, 0, 200, 255];
    let src = img(1, 1, [0; 4], &px);
    let mut buf = vec![0u32; 9];
    let mut s = Surface { px: &mut buf, width: 3, height: 3, stride: 3 };
    // Half a pixel off the grid: four pixels share it, a quarter each.
    s.turned(&src, (1.0, 1.0), 0.0, 1.0);
    let reds: Vec<u32> = buf.iter().map(|p| p >> 16 & 0xFF).collect();
    assert_eq!(reds, vec![50, 50, 0, 50, 50, 0, 0, 0, 0]);
}

#[test]
fn screen_lightens_at_its_opacity() {
    let white = [255u8, 255, 255, 255];
    let black = [0u8, 0, 0, 255];
    let mut buf = vec![0x000000u32, 0x808080, 0x204060];
    let mut s = Surface { px: &mut buf, width: 3, height: 1, stride: 3 };
    let two_white = [white, white].concat();
    s.screen(&img(2, 1, [0; 4], &two_white), 0, 0, 2, 1, 255);
    assert_eq!(&buf[..2], &[0xFFFFFF, 0xFFFFFF], "white screened at full: white");
    // Laid 1:1, never stretched (TASK-090A): a box wider than the picture
    // keeps what was under the rest.
    let mut wide = vec![0x101010u32; 3];
    let mut s = Surface { px: &mut wide, width: 3, height: 1, stride: 3 };
    s.screen(&img(1, 1, [0; 4], &white), 0, 0, 3, 1, 255);
    assert_eq!(wide, [0xFFFFFF, 0x101010, 0x101010]);
    let mut s = Surface { px: &mut buf, width: 3, height: 1, stride: 3 };
    s.screen(&img(1, 1, [0; 4], &black), 2, 0, 1, 1, 255);
    assert_eq!(buf[2], 0x204060, "black screens to nothing");
    let mut dark = vec![0u32];
    let mut s = Surface { px: &mut dark, width: 1, height: 1, stride: 1 };
    s.screen(&img(1, 1, [0; 4], &white), 0, 0, 1, 1, 102);
    assert_eq!(dark[0], 0x666666, "40 %");
}

fn image(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> alloc::vec::Vec<u8> {
    let mut px = alloc::vec::Vec::new();
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = f(x, y);
            px.extend_from_slice(&[b, g, r, 255]);
        }
    }
    crate::fimg::encode(w, h, 4, [0; 4], &px)
}

#[test]
fn cover_keeps_a_screen_sized_source_and_fills_any_other() {
    let bytes = image(8, 5, |x, y| [(x * 30) as u8, (y * 50) as u8, 7]);
    let src = crate::fimg::parse(&bytes).unwrap();
    let same = cover(&src, 8, 5);
    for y in 0..5 {
        for x in 0..8 {
            let want = 0xFF00_0000 | (x * 30) << 16 | (y * 50) << 8 | 7;
            assert_eq!(same[(y * 8 + x) as usize], want, "({x}, {y}) as it was");
        }
    }
    // Twice the size: every pixel filled, the corners the source's corners.
    let big = cover(&src, 16, 10);
    assert_eq!(big.len(), 160);
    assert_eq!(big[0] & 0xFFFFFF, 7, "the top-left stays the source's");
    assert_eq!(big[159] & 0xFFFFFF, (7 * 30) << 16 | (4 * 50) << 8 | 7, "the bottom-right too");
    // A taller screen: the sides cropped, the middle column still the
    // source's middle (columns 3 and 4 between them).
    let tall = cover(&src, 4, 5);
    let mid = tall[2] >> 16 & 0xFF;
    assert!((90..=150).contains(&mid), "the middle of the source, not its edge: {mid}");
    // A wider screen: the rows cropped, the left edge the source's left.
    let wide = cover(&src, 16, 5);
    assert_eq!(wide[0] >> 16 & 0xFF, 0);
}

#[test]
fn cover_brings_a_larger_source_down_by_area() {
    // 4 × 2 → 2 × 1: each screen pixel the mean of the 2 × 2 under it.
    let bytes = image(4, 2, |x, y| [(x * 40 + y * 20) as u8, 100, (x * 10) as u8]);
    let src = crate::fimg::parse(&bytes).unwrap();
    let half = cover(&src, 2, 1);
    // Left: red (0, 40, 20, 60) → 30; blue (0, 10, 0, 10) → 5.
    assert_eq!(half[0] & 0xFF_FFFF, 30 << 16 | 100 << 8 | 5);
    // Right: red (80, 120, 100, 140) → 110; blue (20, 30, 20, 30) → 25.
    assert_eq!(half[1] & 0xFF_FFFF, 110 << 16 | 100 << 8 | 25);
    // 6 × 3 → 4 × 2 (two thirds): the first screen pixel covers source
    // column 0 fully and column 1 by half, rows the same way.
    let bytes = image(6, 3, |x, _| [(x * 30) as u8, 0, 0]);
    let src = crate::fimg::parse(&bytes).unwrap();
    let two_thirds = cover(&src, 4, 2);
    // Columns 0 and 1 weighted 1 : 0.5: (0 + 15) / 1.5 = 10.
    assert_eq!(two_thirds[0] >> 16 & 0xFF, 10);
    // Columns 1 (by half) and 2: (15 + 60) / 1.5 = 50.
    assert_eq!(two_thirds[1] >> 16 & 0xFF, 50);
    // Never a sample from outside: a uniform source stays uniform.
    let flat = image(2560, 1600, |_, _| [17, 34, 51]);
    let src = crate::fimg::parse(&flat).unwrap();
    assert!(
        cover(&src, 1280, 800).iter().chain(&cover(&src, 1366, 768)).all(|&p| p & 0xFF_FFFF == 17 << 16 | 34 << 8 | 51)
    );
}
