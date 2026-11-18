// SPDX-License-Identifier: Apache-2.0
//! Colours as the UI kit writes them: sRGB, 8 bits a channel, straight
//! (not premultiplied) alpha.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Rgba {
        Rgba { r, g, b, a }
    }

    /// As a framebuffer pixel, 0x00RRGGBB (opaque colours only).
    pub const fn xrgb(self) -> u32 {
        (self.r as u32) << 16 | (self.g as u32) << 8 | self.b as u32
    }
}
