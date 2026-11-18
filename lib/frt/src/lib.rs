// SPDX-License-Identifier: Apache-2.0
//! frt, the Freally user runtime (TASK-029): the entry point, the system-call
//! wrappers, the heap (pages from VMOs mapped into the process's own address
//! space), and the panic handler, which ends in a `Panic` fault report.
//!
//! A program uses `fstd::program!(run);` with `fn run(bootstrap: u32) -> i64`.
//!
//! The runtime pieces (`_start`, the allocator, the panic handler) exist only
//! when built for Freally OS (`target_os = "none"` with the x86_64-freally
//! target); on the host the crate is just the wrappers, so the workspace's
//! host gates still build and test everything above it.
#![no_std]

#[cfg(target_os = "none")]
mod heap;
pub mod sys;

pub use fcore_abi::{FErr, well_known};

#[cfg(target_os = "none")]
core::arch::global_asm!(
    ".global _start",
    "_start:",
    // The kernel enters with RDI = the bootstrap argument and RSP at the stack
    // top; end the frame-pointer chain here and align for the call.
    "    xor ebp, ebp",
    "    and rsp, -16",
    "    call _frt_main",
    "    ud2",
);

/// A panic ends in `ud2` with the message in RDI/RSI and the marker in RAX: the
/// kernel files a `Panic` fault report with the message, then ends the process.
#[cfg(target_os = "none")]
#[panic_handler]
fn panic(p: &core::panic::PanicInfo) -> ! {
    let mut buf = [0u8; 512];
    let mut w = Writer { b: &mut buf, n: 0 };
    let _ = core::fmt::write(&mut w, format_args!("{}", p.message()));
    if let Some(l) = p.location() {
        let _ = core::fmt::write(&mut w, format_args!(" ({}:{})", l.file(), l.line()));
    }
    let n = w.n;
    // SAFETY: never returns; the kernel reads the message through its user-copy path.
    unsafe {
        core::arch::asm!(
            "ud2",
            in("rax") fcore_abi::USER_PANIC_MAGIC,
            in("rdi") buf.as_ptr(),
            in("rsi") n,
            options(noreturn, nostack),
        )
    }
}

#[cfg(target_os = "none")]
struct Writer<'a> {
    b: &'a mut [u8],
    n: usize,
}

#[cfg(target_os = "none")]
impl core::fmt::Write for Writer<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let mut take = s.len().min(self.b.len() - self.n);
        while !s.is_char_boundary(take) {
            take -= 1;
        }
        self.b[self.n..self.n + take].copy_from_slice(&s.as_bytes()[..take]);
        self.n += take;
        Ok(())
    }
}
