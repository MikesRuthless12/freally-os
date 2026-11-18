// SPDX-License-Identifier: Apache-2.0
//! Golden tests: compositing, clipping, 9-slice, FIMG and the downscale,
//! each pixel worked out by hand.

extern crate std;

use super::*;
use crate::fimg::{self, Fimg};
use alloc::vec;
use alloc::vec::Vec;

fn img(w: u32, h: u32, slice: [u16; 4], px: &[u8]) -> Fimg<'_> {
    Fimg { width: w, height: h, scale: 1, slice, pixels: px }
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
    let bytes = fimg::encode(2, 3, 2, [0, 1, 0, 1], &px);
    assert_eq!(bytes.len(), fimg::HEADER_LEN + px.len());
    let f = fimg::parse(&bytes).unwrap();
    assert_eq!((f.width, f.height, f.scale, f.slice), (2, 3, 2, [0, 1, 0, 1]));
    assert_eq!(f.pixels, &px[..]);
    assert!(fimg::parse(&bytes[..bytes.len() - 1]).is_err());
    let mut bad = bytes.clone();
    bad[16] = 4; // scale 4
    assert!(fimg::parse(&bad).is_err());
    let wide = fimg::encode(2, 3, 1, [2, 0, 1, 0], &px); // insets 3 > width 2
    assert!(fimg::parse(&wide).is_err());
}

#[test]
fn rgba_becomes_premultiplied_bgra() {
    assert_eq!(fimg::from_rgba(&[200, 100, 50, 255]), [50, 100, 200, 255]);
    assert_eq!(fimg::from_rgba(&[200, 100, 50, 0]), [0, 0, 0, 0]);
    assert_eq!(fimg::from_rgba(&[255, 255, 255, 128]), [128, 128, 128, 128]);
}

#[test]
fn downscale_averages_by_area() {
    // 3×3 → 1×1: the mean of all nine.
    let px: Vec<u8> = (0..9u8).flat_map(|i| [i * 10, 0, 0, 255]).collect();
    let (w, h, out) = fimg::downscale(3, 3, &px, 1, 3);
    assert_eq!((w, h), (1, 1));
    assert_eq!(out, [40, 0, 0, 255]);
    // 3×3 → 2×2 (2/3): the top-left target covers source (0,0) fully,
    // (1,0) and (0,1) by half, (1,1) by a quarter: (0 + 5 + 15 + 10) / 2.25.
    let (w, h, out) = fimg::downscale(3, 3, &px, 2, 3);
    assert_eq!((w, h), (2, 2));
    assert_eq!(out[0], ((0.0 + 10.0 * 0.5 + 30.0 * 0.5 + 40.0 * 0.25) / 2.25f64).round() as u8);
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
    s.screen(&img(1, 1, [0; 4], &white), 0, 0, 2, 1, 255);
    assert_eq!(&buf[..2], &[0xFFFFFF, 0xFFFFFF], "white screened at full: white");
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
    crate::fimg::encode(w, h, 1, [0; 4], &px)
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
