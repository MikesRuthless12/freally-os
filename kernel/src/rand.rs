// SPDX-License-Identifier: Apache-2.0
//! Kernel randomness for unguessable values (handle numbers, later ASLR):
//! RDRAND when the CPU has it, mixed into a xorshift state seeded from the TSC.
//! Not for cryptographic keys.

use core::sync::atomic::{AtomicU64, Ordering};

static STATE: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);

fn rdrand() -> Option<u64> {
    if crate::arch::cpu::cpuid(1, 0).ecx & (1 << 30) == 0 {
        return None;
    }
    for _ in 0..10 {
        let (v, ok): (u64, u8);
        // SAFETY: RDRAND exists (CPUID said so); it only writes its operands.
        unsafe { core::arch::asm!("rdrand {}", "setc {}", out(reg) v, out(reg_byte) ok, options(nomem, nostack)) };
        if ok == 1 {
            return Some(v);
        }
    }
    None
}

pub fn u64() -> u64 {
    let mut x = STATE.load(Ordering::Relaxed) ^ crate::arch::cpu::rdtsc() ^ rdrand().unwrap_or(0);
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    STATE.store(x, Ordering::Relaxed);
    x
}

pub fn u32() -> u32 {
    (u64() >> 16) as u32
}
