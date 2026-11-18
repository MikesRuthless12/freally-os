// SPDX-License-Identifier: Apache-2.0
//! The firmware's text console (and, through OVMF, COM1), as `core::fmt::Write`.
//! Used only before the bootloader paints: printing afterwards would draw over it.

use crate::uefi::SimpleTextOutput;
use core::fmt;
use core::sync::atomic::{AtomicPtr, Ordering};

static OUT: AtomicPtr<SimpleTextOutput> = AtomicPtr::new(core::ptr::null_mut());

pub fn init(out: *mut SimpleTextOutput) {
    OUT.store(out, Ordering::Relaxed);
}

/// Hides the firmware's text cursor, so it never blinks over a painted screen.
pub fn hide_cursor() {
    let out = OUT.load(Ordering::Relaxed);
    if !out.is_null() {
        // SAFETY: `out` is the firmware's ConOut protocol, valid while boot services run.
        unsafe { ((*out).enable_cursor)(out, 0) };
    }
}

/// Writes to the console; `\n` becomes `\r\n`.
pub struct Console;

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let out = OUT.load(Ordering::Relaxed);
        if out.is_null() {
            return Ok(());
        }
        let mut buf = [0u16; 129];
        let mut n = 0;
        let mut flush = |buf: &mut [u16; 129], n: &mut usize| {
            buf[*n] = 0;
            // SAFETY: `buf` is NUL-terminated UTF-16; `out` is the firmware's ConOut.
            unsafe { ((*out).output_string)(out, buf.as_ptr()) };
            *n = 0;
        };
        for c in s.chars() {
            if c == '\n' {
                if n + 2 > 128 {
                    flush(&mut buf, &mut n);
                }
                buf[n] = b'\r' as u16;
                buf[n + 1] = b'\n' as u16;
                n += 2;
                continue;
            }
            let mut units = [0u16; 2];
            let units = c.encode_utf16(&mut units);
            if n + units.len() > 128 {
                flush(&mut buf, &mut n);
            }
            for u in units.iter() {
                buf[n] = *u;
                n += 1;
            }
        }
        if n > 0 {
            flush(&mut buf, &mut n);
        }
        Ok(())
    }
}

macro_rules! say {
    ($($arg:tt)*) => {{
        let _ = core::fmt::Write::write_fmt(&mut crate::console::Console, format_args!($($arg)*));
        let _ = core::fmt::Write::write_str(&mut crate::console::Console, "\n");
    }};
}
