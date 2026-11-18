// SPDX-License-Identifier: Apache-2.0
//! Puts a screen on the display through the firmware's GOP. It honours every
//! pixel format (RGB, BGR, bit masks, and Blt-only through `Blt`) and the stride
//! (`PixelsPerScanLine`), which may be wider than the visible width.

use crate::uefi::*;
use core::ffi::c_void;
use core::ptr::null_mut;

/// What was found on the display.
#[derive(Clone, Copy, Debug)]
pub struct Display {
    pub w: usize,
    pub h: usize,
    pub stride: usize,
    pub format: u32,
}

/// The firmware's graphics output, if it has one.
///
/// # Safety
/// `bs` must be the firmware's boot services, still running.
pub unsafe fn gop(bs: &BootServices) -> Option<(*mut GraphicsOutput, Display)> {
    let mut g: *mut c_void = null_mut();
    if is_error((bs.locate_protocol)(&GRAPHICS_OUTPUT_GUID, null_mut(), &mut g)) || g.is_null() {
        return None;
    }
    let g = g as *mut GraphicsOutput;
    // SAFETY: the firmware's GOP and its current mode.
    let info = unsafe { &*(*(*g).mode).info };
    Some((
        g,
        Display {
            w: info.horizontal_resolution as usize,
            h: info.vertical_resolution as usize,
            stride: info.pixels_per_scan_line as usize,
            format: info.pixel_format,
        },
    ))
}

/// Packs `0x00RRGGBB` into a bit-mask pixel format.
fn pack_mask(rgb: u32, masks: [u32; 3]) -> u32 {
    let mut out = 0;
    for (i, &m) in masks.iter().enumerate() {
        if m == 0 {
            continue;
        }
        let c = (rgb >> (16 - 8 * i)) & 0xFF;
        let bits = m.count_ones();
        let v = if bits >= 8 { c << (bits - 8) } else { c >> (8 - bits) };
        out |= (v << m.trailing_zeros()) & m;
    }
    out
}

/// Copies `px` (`0x00RRGGBB`, `d.w × d.h`) to the screen.
///
/// # Safety
/// `g` must be the firmware's GOP, with `d` read from its current mode.
pub unsafe fn present(g: *mut GraphicsOutput, d: Display, px: &mut [u32]) {
    // SAFETY: the firmware's GOP and its current mode.
    let (mode, info) = unsafe { (&*(*g).mode, &*(*(*g).mode).info) };
    let fb = mode.frame_buffer_base as *mut u32;
    let pixel = |rgb: u32| -> u32 {
        match d.format {
            0 => ((rgb >> 16) & 0xFF) | (rgb & 0xFF00) | ((rgb & 0xFF) << 16),
            1 => rgb,
            _ => pack_mask(rgb, [info.pixel_information[0], info.pixel_information[1], info.pixel_information[2]]),
        }
    };
    if d.format == 3 || fb.is_null() {
        // Blt-only: EFI_GRAPHICS_OUTPUT_BLT_PIXEL is blue, green, red, reserved,
        // which is exactly 0x00RRGGBB read as a little-endian u32.
        // SAFETY: `px` holds d.w × d.h pixels, rows d.w apart.
        unsafe {
            ((*g).blt)(g, px.as_mut_ptr(), BLT_BUFFER_TO_VIDEO, 0, 0, 0, 0, d.w, d.h, d.w * 4);
        }
        return;
    }
    for y in 0..d.h {
        for x in 0..d.w {
            // SAFETY: (x, y) is inside the visible mode; rows are `stride` pixels apart
            // in a framebuffer of `frame_buffer_size` bytes.
            unsafe { core::ptr::write_volatile(fb.add(y * d.stride + x), pixel(px[y * d.w + x])) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::pack_mask;

    #[test]
    fn masks_pack_like_the_fixed_formats() {
        // BGRX as masks: red 0x00FF0000, green 0x0000FF00, blue 0x000000FF.
        assert_eq!(pack_mask(0x79D920, [0x00FF_0000, 0x0000_FF00, 0x0000_00FF]), 0x79D920);
        // RGB565.
        assert_eq!(pack_mask(0xFFFFFF, [0xF800, 0x07E0, 0x001F]), 0xFFFF);
    }
}
