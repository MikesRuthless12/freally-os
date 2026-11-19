// SPDX-License-Identifier: Apache-2.0
//! The crisp guard (TASK-090A): every picture on screen is drawn from the
//! art made for the screen's scale, 1:1. With the `guard` feature (the e2e
//! and dev images, the tests) the blitter keeps the last picture that broke
//! that and counts them:
//!
//! - **made for another scale:** a program says the scale it draws at
//!   ([`expect_scale`]); a FIMG made for any other, drawn, is caught (the
//!   wallpaper, brought down to any screen by [`cover`](crate::blit::cover),
//!   is the one picture exempt);
//! - **drawn larger than it is:** the wallpaper on a screen larger than its
//!   render, or a picture with no 9-slice insets stretched by `nine`. (A
//!   9-slice's corners are never drawn larger, and its edges and middle
//!   stretch along themselves by design.)
//!
//! The program asks with [`take`] after a frame and says so (stage and the
//! desktop log an error naming the piece, which fails the CDP guard).
//! Without the feature the checks are compiled out and [`take`] has nothing
//! to give.

use crate::fimg::Fimg;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

/// A picture the guard caught: its size and the scale it was made for (in
/// quarters, as FIMG keeps it), the size it was shown at, the scale the
/// program draws at (0: not said), and where its pixels are (so a program
/// can find which piece of its bundles it was).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caught {
    pub w: u32,
    pub h: u32,
    pub scale: u8,
    pub dw: u32,
    pub dh: u32,
    pub want_pct: u32,
    pub pixels: usize,
}

static COUNT: AtomicU32 = AtomicU32::new(0);
static WANT: AtomicU32 = AtomicU32::new(0);
static LAST: [AtomicUsize; 7] = [const { AtomicUsize::new(0) }; 7];

/// The scale, in percent, this program draws at from now on (0: any).
pub fn expect_scale(pct: u32) {
    WANT.store(pct, Ordering::Relaxed);
}

#[cfg(any(test, feature = "guard"))]
fn keep(src: &Fimg, w: u32, h: u32, dw: u32, dh: u32, want: u32) {
    let v = [
        w as usize,
        h as usize,
        src.scale as usize,
        dw as usize,
        dh as usize,
        want as usize,
        src.pixels.as_ptr() as usize,
    ];
    for (slot, v) in LAST.iter().zip(v) {
        slot.store(v, Ordering::Relaxed);
    }
    COUNT.fetch_add(1, Ordering::Relaxed);
}

/// `src` drawn: caught if it was made for a scale other than the one the
/// program said.
#[inline]
#[allow(unused_variables)]
pub(crate) fn check_scale(src: &Fimg) {
    #[cfg(any(test, feature = "guard"))]
    {
        let want = WANT.load(Ordering::Relaxed);
        if want != 0 && src.scale as u32 * 25 != want {
            keep(src, src.width, src.height, src.width, src.height, want);
        }
    }
}

/// `src`'s `w × h` shown at `dw × dh`: caught if that's larger either way.
#[inline]
#[allow(unused_variables)]
pub(crate) fn check_size(src: &Fimg, w: u32, h: u32, dw: u32, dh: u32) {
    #[cfg(any(test, feature = "guard"))]
    if dw > w || dh > h {
        keep(src, w, h, dw, dh, 0);
    }
}

/// How many pictures were caught since the last [`take`], and the last.
pub fn take() -> Option<(u32, Caught)> {
    let n = COUNT.swap(0, Ordering::Relaxed);
    if n == 0 {
        return None;
    }
    let v = LAST.each_ref().map(|a| a.load(Ordering::Relaxed));
    Some((
        n,
        Caught {
            w: v[0] as u32,
            h: v[1] as u32,
            scale: v[2] as u8,
            dw: v[3] as u32,
            dh: v[4] as u32,
            want_pct: v[5] as u32,
            pixels: v[6],
        },
    ))
}

/// A caught picture in words, for a log line: `{name} (made for 100 %) on
/// a 150 % screen` or `{name} (40x40, for 150 %) drawn at 60x60`.
pub fn describe(name: &str, c: &Caught) -> alloc::string::String {
    if c.want_pct != 0 {
        alloc::format!("{name} (made for {} %) on a {} % screen", c.scale as u32 * 25, c.want_pct)
    } else {
        alloc::format!("{name} ({}x{}, for {} %) drawn at {}x{}", c.w, c.h, c.scale as u32 * 25, c.dw, c.dh)
    }
}
