// SPDX-License-Identifier: Apache-2.0
//! The boot timeline (PRD § 3): raw TSC stamps per stage, B0 → S4.

use core::sync::atomic::{AtomicU64, Ordering};

pub const B0: u8 = 2;
pub const B1: u8 = 3;
pub const B2: u8 = 4;
pub const B3: u8 = 5;
pub const K0: u8 = 6;
pub const K1: u8 = 7;
pub const K2: u8 = 8;
pub const K3: u8 = 9;
pub const C0: u8 = 10;

static STAMPS: [AtomicU64; 15] = [const { AtomicU64::new(0) }; 15];

pub fn stamp(id: u8, tsc: u64) {
    if let Some(s) = STAMPS.get(id as usize) {
        let _ = s.compare_exchange(0, tsc, Ordering::Relaxed, Ordering::Relaxed);
    }
}

pub fn get(id: u8) -> u64 {
    STAMPS.get(id as usize).map_or(0, |s| s.load(Ordering::Relaxed))
}

/// Every stage stamped so far, in order.
pub fn stamps() -> alloc::vec::Vec<(u8, u64)> {
    (0..STAMPS.len() as u8).map(|i| (i, get(i))).filter(|(_, t)| *t != 0).collect()
}

/// C0: conductor (the first user process) reached user mode.
pub fn first_user_entry() {
    let now = crate::arch::cpu::rdtsc();
    if get(C0) == 0 {
        stamp(C0, now);
        let b0 = get(B0);
        klog!(
            "freally-core: conductor started (C0 {} ms after B0)",
            crate::time::ticks_to_ns(now.saturating_sub(b0)) / 1_000_000
        );
    }
}
