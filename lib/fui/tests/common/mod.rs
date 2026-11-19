// SPDX-License-Identifier: Apache-2.0
//! The hardware set for the tests: each piece's PNG from `assets/hardware`,
//! made FIMG the way `cargo xtask assets` makes it, once per theme.
#![allow(dead_code)]

use fui::hardware::{Hardware, RENDER_SCALE, slice_of};
use std::sync::OnceLock;

/// The set for `theme` at `scale` percent, made as `cargo xtask assets` makes it.
pub fn build(theme: &str, scale: u32) -> Hardware<'static> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/hardware");
    Hardware::load(scale, |piece| {
        let png = std::fs::read(format!("{dir}/{theme}/{piece}.png")).ok()?;
        let file = fimage::fimg::from_png(&png, scale, RENDER_SCALE, slice_of(piece)).ok()?;
        Some(Box::leak(file.into_boxed_slice()))
    })
    .unwrap()
}

pub fn dark() -> &'static Hardware<'static> {
    static H: OnceLock<Hardware<'static>> = OnceLock::new();
    H.get_or_init(|| build("dark", 100))
}

pub fn light() -> &'static Hardware<'static> {
    static H: OnceLock<Hardware<'static>> = OnceLock::new();
    H.get_or_init(|| build("light", 100))
}

pub fn dark_2x() -> &'static Hardware<'static> {
    static H: OnceLock<Hardware<'static>> = OnceLock::new();
    H.get_or_init(|| build("dark", 200))
}

/// Clearly Freally green (from the deep accent to the lit track's lime):
/// green the strongest, well above blue and clear of red.
pub fn greenish(p: u32) -> bool {
    let (r, g, b) = (p >> 16 & 0xFF, p >> 8 & 0xFF, p & 0xFF);
    g >= r + 30 && g > b + 60
}

pub fn luma(p: u32) -> u32 {
    let (r, g, b) = (p >> 16 & 0xFF, p >> 8 & 0xFF, p & 0xFF);
    (r * 299 + g * 587 + b * 114) / 1000
}
