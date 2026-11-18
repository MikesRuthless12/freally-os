// SPDX-License-Identifier: Apache-2.0
//! The kernel's early log on COM1 (port 0x3F8), dev and e2e flavours only:
//! the release flavour has no serial log, so this compiles to nothing there.

use core::fmt;

#[cfg(not(feature = "flavour-release"))]
fn out(b: u8) {
    // SAFETY: COM1's line-status and data ports; the firmware configured the UART.
    unsafe {
        loop {
            let lsr: u8;
            core::arch::asm!("in al, dx", out("al") lsr, in("dx") 0x3FDu16, options(nomem, nostack, preserves_flags));
            if lsr & 0x20 != 0 {
                break;
            }
        }
        core::arch::asm!("out dx, al", in("al") b, in("dx") 0x3F8u16, options(nomem, nostack, preserves_flags));
    }
}

#[cfg(feature = "flavour-release")]
fn out(_: u8) {}

pub struct Com1;

impl fmt::Write for Com1 {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                out(b'\r');
            }
            out(b);
        }
        Ok(())
    }
}

/// `klog!("…")`: one line on COM1 (nothing in the release flavour).
#[macro_export]
macro_rules! klog {
    ($($arg:tt)*) => {{
        let _ = core::fmt::Write::write_fmt(&mut $crate::serial::Com1, format_args!($($arg)*));
        let _ = core::fmt::Write::write_str(&mut $crate::serial::Com1, "\n");
    }};
}
