// SPDX-License-Identifier: Apache-2.0
//! The kernel's early log: COM1 (port 0x3F8) in the dev and e2e flavours (the
//! release flavour has no serial log), and in every flavour a 64 KiB ring the
//! logger drains once it starts (`system_info(KERNEL_LOG)`, TASK-039).

use core::fmt;
use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};

/// The early ring: bytes, oldest overwritten first. Lock-free (a fault handler
/// may log while another log line is half written).
pub const RING_SIZE: usize = 64 * 1024;
static RING: [AtomicU8; RING_SIZE] = [const { AtomicU8::new(0) }; RING_SIZE];
/// Bytes ever written; the ring holds the last `RING_SIZE` of them.
static WRITTEN: AtomicU64 = AtomicU64::new(0);

fn ring(b: u8) {
    let at = WRITTEN.fetch_add(1, Ordering::Relaxed);
    RING[(at % RING_SIZE as u64) as usize].store(b, Ordering::Relaxed);
}

/// The ring as `system_info(KERNEL_LOG)` returns it: the total ever written
/// (u64), then the bytes still held, oldest first.
pub fn ring_snapshot() -> alloc::vec::Vec<u8> {
    let total = WRITTEN.load(Ordering::Acquire);
    let held = total.min(RING_SIZE as u64);
    let mut out = alloc::vec::Vec::with_capacity(8 + held as usize);
    out.extend_from_slice(&total.to_le_bytes());
    for i in total - held..total {
        out.push(RING[(i % RING_SIZE as u64) as usize].load(Ordering::Relaxed));
    }
    out
}

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
            ring(b);
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
