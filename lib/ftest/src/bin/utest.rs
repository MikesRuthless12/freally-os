// SPDX-License-Identifier: Apache-2.0
//! The user test program the kernel's harness runs (`cargo xtask test`). Its
//! argument picks what it does:
//! - 0: checks the runtime (heap, fwire, the clock, a channel, an event, a
//!   futex sleep) and exits 42 if all pass, else the number of the first check
//!   that failed;
//! - 1: reads address 0 (the kernel files a page-fault report);
//! - 2: panics (the kernel files a panic report with the message).
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

fstd::program!(run);

fn run(mode: u32) -> i64 {
    match mode {
        0 => ftest::checks(),
        1 => {
            null_read();
            1
        }
        _ => panic!("utest: deliberate panic"),
    }
}

/// A real load from address 0 (inline assembly, so the compiler can't reason it away).
#[inline(never)]
fn null_read() {
    // SAFETY: deliberately faults; the kernel ends the process.
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::asm!("mov {0}, qword ptr [{0}]", inout(reg) 0usize => _, options(nostack));
    }
}
