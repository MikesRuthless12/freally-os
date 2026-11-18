// SPDX-License-Identifier: Apache-2.0
//! Freally's own font code. The fonts are Noto (SIL OFL-1.1, loaded as data:
//! `assets/fonts/noto/`); every line that reads and draws them is here.
//!
//! - [`ttf`]: a TrueType parser (`cmap` 4 and 12, `glyf` simple and composite
//!   glyphs, `hmtx`), with the `raster` feature.
//! - [`raster`]: an exact-area coverage rasterizer: each edge adds the signed
//!   area it covers in each pixel, and a running sum along each row gives the
//!   coverage. Anti-aliased, and deterministic for the same input.
//! - [`text`]: drawing at run time (user space): glyphs rendered on first
//!   use and kept, blended in linear light.
//! - [`atlas`]: the FATL glyph atlas, pre-rendered at build time, and drawing
//!   text from it with no allocation (the bootloader's and panic screen's path).
#![cfg_attr(not(test), no_std)]

#[cfg(feature = "raster")]
extern crate alloc;

pub mod atlas;
#[cfg(feature = "raster")]
mod fmath;
#[cfg(feature = "raster")]
pub mod raster;
#[cfg(feature = "raster")]
pub mod text;
#[cfg(feature = "raster")]
pub mod ttf;
