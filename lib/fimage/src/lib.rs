// SPDX-License-Identifier: Apache-2.0
//! Freally's own image code.
//!
//! - [`png`]: a PNG encoder that writes stored (uncompressed) deflate blocks,
//!   exact and simple, and a decoder for exactly what it writes.
//! - [`ppm`]: reading the binary PPM (P6) QEMU's `screendump` writes.
#![no_std]

extern crate alloc;

pub mod png;
pub mod ppm;
