// SPDX-License-Identifier: Apache-2.0
//! `log=screen` (TASK-074; the dev flavour only, compiled out of release and
//! e2e): the log drawn on the framebuffer from the logger's start (the
//! kernel's early lines included) until stage begins (its "stage: …,
//! starting at …" line). A machine with no serial port that stops before
//! the desktop shows why on its own screen; after that, Monitor's Log tab
//! has every line.
//!
//! Conductor grants the framebuffer and BootInfo only to a dev image booted
//! with `log=screen` in `\FREALLY\CMDLINE.TXT`.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::{BootInfo, PixelFormat, cache, perm, well_known};
use fstd::handle::Handle;

/// The most lines kept for the screen (more than any screen shows).
const KEEP: usize = 200;

pub struct Screen {
    fb: *mut u32,
    _vmo: Handle,
    w: usize,
    h: usize,
    stride: usize,
    rgb: bool,
    back: Vec<u32>,
    lines: VecDeque<(String, u8)>,
    dirty: bool,
    /// Stage has begun: the screen is its now.
    pub done: bool,
}

impl Screen {
    /// The screen, if conductor granted it (a dev image with `log=screen`).
    pub fn open() -> Option<Screen> {
        let res = fstd::service::take_handle("framebuffer")?;
        let bi = fstd::service::take_handle("bootinfo")?;
        let at = fstd::sys::vm_map(well_known::SELF_ASPACE, bi.raw(), 0, 4096, 0, perm::READ).ok()?;
        // SAFETY: conductor's copy of BootInfo, at offset 0 of that page.
        let info: BootInfo = unsafe { core::ptr::read_unaligned(at as *const BootInfo) };
        if !fcore_abi::cmdline::has(&info.cmdline, "log=screen") {
            return None;
        }
        let fb = info.framebuffer;
        let len = fb.size.next_multiple_of(4096);
        let vmo = fstd::sys::vmo_create_physical(res.raw(), fb.base, len, cache::WRITE_COMBINING).ok()?;
        let vmo = Handle::from_raw(vmo)?;
        let px = fstd::sys::vm_map(well_known::SELF_ASPACE, vmo.raw(), 0, len, 0, perm::READ | perm::WRITE).ok()?;
        let (w, h) = (fb.width as usize, fb.height as usize);
        Some(Screen {
            fb: px as *mut u32,
            _vmo: vmo,
            w,
            h,
            stride: fb.stride_px as usize,
            rgb: fb.format == PixelFormat::Rgbx8888,
            back: alloc::vec![0; w * h],
            lines: VecDeque::new(),
            dirty: true,
            done: false,
        })
    }

    /// One more line (until stage begins).
    pub fn line(&mut self, svc: &str, level: u8, msg: &str) {
        if self.done {
            return;
        }
        if svc == "stage" && msg.starts_with("stage: ") && msg.contains(", starting at ") {
            self.done = true;
            return;
        }
        if self.lines.len() == KEEP {
            self.lines.pop_front();
        }
        self.lines.push_back((String::from(msg), level));
        self.dirty = true;
    }

    /// Draws what changed since the last call.
    pub fn flush(&mut self) {
        if self.done || !self.dirty {
            return;
        }
        self.dirty = false;
        let refs: Vec<(&str, u8)> = self.lines.iter().map(|(s, l)| (s.as_str(), *l)).collect();
        fscreen::log(&mut fscreen::Canvas { px: &mut self.back, w: self.w, h: self.h }, &refs);
        for y in 0..self.h {
            for x in 0..self.w {
                let p = self.back[y * self.w + x];
                let v = if self.rgb { ((p >> 16) & 0xFF) | (p & 0xFF00) | ((p & 0xFF) << 16) } else { p };
                // SAFETY: (x, y) is inside the mode; rows are `stride` pixels apart.
                unsafe { core::ptr::write_volatile(self.fb.add(y * self.stride + x), v) };
            }
        }
    }
}
