// SPDX-License-Identifier: Apache-2.0
//! The user test program the kernel's harness runs (`cargo xtask test`). Its
//! argument picks what it does:
//! - 0: checks the runtime (heap, fwire, the clock, a channel, an event, a
//!   futex sleep) and exits 42 if all pass, else the number of the first check
//!   that failed;
//! - 1: reads address 0 (the kernel files a page-fault report);
//! - 2: panics (the kernel files a panic report with the message);
//! - 3: opens COM1's ports through the resource in handle 4, reads one, exits 7;
//! - 4: reads that port with no grant (a general-protection fault).
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
        // COM1's line status, after opening COM1's ports with the resource in handle 4.
        3 => {
            if fstd::sys::call1(fcore_abi::syscall::IOPORT_GRANT, [4, 0x3F8, 8, 0, 0, 0]).is_err() {
                return 3;
            }
            port_read(0x3FD);
            7
        }
        // The same read with no grant: the kernel faults the process.
        4 => {
            port_read(0x3FD);
            4
        }
        _ => panic!("utest: deliberate panic"),
    }
}

/// One `in` from `port`.
#[inline(never)]
fn port_read(port: u16) -> u8 {
    let v: u8;
    // SAFETY: a port read; without a grant the kernel ends the process.
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        v = 0;
    }
    v
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
