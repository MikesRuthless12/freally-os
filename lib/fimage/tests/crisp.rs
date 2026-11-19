// SPDX-License-Identifier: Apache-2.0
//! TASK-090A: a render brought to each UI scale the way a camera would see
//! it (an area filter in linear light, alpha premultiplied), and FIMG made
//! for every scale the OS offers.
#![cfg(feature = "decode")]

use fimage::fimg;

#[test]
fn black_and_white_average_in_light_not_in_numbers() {
    // Half black, half white: half the light, which sRGB writes as 188, not 128.
    let (w, h, px) = fimg::downscale_linear(2, 1, &[0, 0, 0, 255, 255, 255, 255, 255], 1, 2);
    assert_eq!((w, h), (1, 1));
    assert_eq!(px, [188, 188, 188, 255]);
}

#[test]
fn an_edge_neither_darkens_nor_fringes() {
    // White beside nothing: white at half cover (premultiplied 128s), never grey.
    let (_, _, px) = fimg::downscale_linear(2, 1, &[255, 255, 255, 255, 0, 0, 0, 0], 1, 2);
    assert_eq!(px, [128, 128, 128, 128]);
    // Red beside a transparent pixel whose colour is green: no green leaks in.
    let (_, _, px) = fimg::downscale_linear(2, 1, &[255, 0, 0, 255, 0, 255, 0, 0], 1, 2);
    assert_eq!(px, [0, 0, 128, 128], "BGRA: red at half cover, nothing else");
}

#[test]
fn every_scale_the_os_offers_and_no_other() {
    // A 4x render 40 × 20, opaque grey, a 9-slice of 8 all round.
    let png = fimage::png::encode(40, 20, 4, &[90u8, 90, 90, 255].repeat(40 * 20));
    for pct in (100..=300).step_by(25) {
        let bytes = fimg::from_png(&png, pct, 4, [8; 4]).unwrap();
        let f = fimg::parse(&bytes).unwrap();
        assert_eq!(f.scale as u32 * 25, pct);
        assert_eq!((f.width, f.height), ((40 * pct).div_ceil(400), (20 * pct).div_ceil(400)), "{pct} %");
        assert_eq!(f.slice[0] as u32, (8 * pct + 200) / 400, "the insets scale with it at {pct} %");
        assert!(f.pixels.chunks(4).all(|p| p == [90, 90, 90, 255]), "a flat grey stays exactly that grey at {pct} %");
    }
    for pct in [75, 110, 325, 400] {
        assert!(fimg::from_png(&png, pct, 4, [0; 4]).is_err(), "{pct} %");
    }
    // Never above the render: a 2x render can't make 250 %.
    assert!(fimg::from_png(&png, 250, 2, [0; 4]).is_err());
    // At the render's own scale the pixels go in untouched.
    let rgba: Vec<u8> = (0..16u8).flat_map(|i| [i * 16, 255 - i * 16, i, 255]).collect();
    let png = fimage::png::encode(4, 4, 4, &rgba);
    let f = fimg::from_png(&png, 200, 2, [0; 4]).unwrap();
    assert_eq!(&f[fimg::HEADER_LEN..], &fimg::from_rgba(&rgba)[..]);
}
