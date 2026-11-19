// SPDX-License-Identifier: Apache-2.0
//! Freally's own image code.
//!
//! - [`png`]: a PNG encoder, in stored (uncompressed) deflate blocks, exact
//!   and simple, or [`deflate`]d for the Brain's screenshots; and a decoder
//!   for exactly what the stored one writes.
//! - [`ppm`]: reading the binary PPM (P6) QEMU's `screendump` writes.
//! - [`fimg`]: FIMG, Freally's own pixel format (premultiplied BGRA, no
//!   decoding), and the 2x and 1x made from a 3x render.
//! - [`blit`]: drawing a FIMG on a framebuffer (premultiplied over), and
//!   9-slice.
//! - [`inflate`] and [`png::decode`] (feature `decode`): reading any 8-bit
//!   PNG, for `xtask` only: the image carries no outside-format decoder.
#![no_std]

extern crate alloc;

pub mod blit;
pub mod bundle;
pub mod deflate;
pub mod fimg;
#[cfg(feature = "decode")]
pub mod inflate;
pub mod png;
pub mod ppm;
