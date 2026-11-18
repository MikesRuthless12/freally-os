// SPDX-License-Identifier: Apache-2.0
//! The firmware's framebuffer, drawn on directly through the direct map: for
//! the kernel's own screens (the boot note, the panic screen) before any driver.

use fcore_abi::layout::DIRECT_MAP;
use fcore_abi::{Framebuffer, PixelFormat};
use ffont::atlas::Target;

pub struct Fb {
    base: *mut u32,
    w: usize,
    h: usize,
    stride: usize,
    rgb: bool,
}

impl Fb {
    /// The framebuffer the bootloader found, if there is a linear one.
    pub fn new(f: &Framebuffer) -> Option<Fb> {
        if f.base == 0 || f.width == 0 || f.height == 0 {
            return None;
        }
        Some(Fb {
            base: (f.base + DIRECT_MAP) as *mut u32,
            w: f.width as usize,
            h: f.height as usize,
            stride: f.stride_px as usize,
            rgb: f.format == PixelFormat::Rgbx8888,
        })
    }

    fn encode(&self, rgb: u32) -> u32 {
        if self.rgb { ((rgb >> 16) & 0xFF) | (rgb & 0xFF00) | ((rgb & 0xFF) << 16) } else { rgb & 0x00FF_FFFF }
    }
}

impl Target for Fb {
    fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    fn get(&self, x: usize, y: usize) -> u32 {
        // SAFETY: (x, y) is inside the mode; the direct map covers the framebuffer.
        let v = unsafe { core::ptr::read_volatile(self.base.add(y * self.stride + x)) };
        self.encode(v) // the encoding is its own inverse
    }

    fn set(&mut self, x: usize, y: usize, rgb: u32) {
        let v = self.encode(rgb);
        // SAFETY: as above.
        unsafe { core::ptr::write_volatile(self.base.add(y * self.stride + x), v) };
    }
}
