// SPDX-License-Identifier: Apache-2.0
//! Time (TASK-023): the TSC is the clock, its frequency found from CPUID leaf
//! 0x15 (or 0x16), else measured against the HPET, else the PIT. The HPET also
//! checks a CPUID answer. The CMOS RTC gives the date once, at boot; UTC is that
//! plus the monotonic clock.
//!
//! - `MONOTONIC`: nanoseconds since the kernel's clock started; never backwards.
//! - `BOOT`: nanoseconds since the bootloader started (B0).
//! - `UTC`: nanoseconds since 1970, from the RTC at boot.

use crate::arch::cpu;
use core::sync::atomic::{AtomicU64, Ordering};
use fcore_abi::layout::DIRECT_MAP;

static TSC_HZ: AtomicU64 = AtomicU64::new(0);
static TSC_ZERO: AtomicU64 = AtomicU64::new(0);
static B0: AtomicU64 = AtomicU64::new(0);
static RTC_UNIX: AtomicU64 = AtomicU64::new(0);
static HPET: AtomicU64 = AtomicU64::new(0);

pub fn tsc_hz() -> u64 {
    TSC_HZ.load(Ordering::Relaxed)
}

/// TSC ticks to nanoseconds.
pub fn ticks_to_ns(t: u64) -> u64 {
    let hz = tsc_hz().max(1);
    ((t as u128 * 1_000_000_000) / hz as u128) as u64
}

pub fn ns_to_ticks(ns: u64) -> u64 {
    ((ns as u128 * tsc_hz() as u128) / 1_000_000_000) as u64
}

/// Monotonic nanoseconds.
pub fn now_ns() -> u64 {
    ticks_to_ns(cpu::rdtsc().saturating_sub(TSC_ZERO.load(Ordering::Relaxed)))
}

pub fn boot_ns() -> u64 {
    ticks_to_ns(cpu::rdtsc().saturating_sub(B0.load(Ordering::Relaxed)))
}

pub fn utc_ns() -> u64 {
    RTC_UNIX.load(Ordering::Relaxed) * 1_000_000_000 + now_ns()
}

// The HPET: capabilities at 0x0 (period in femtoseconds in the top half),
// configuration at 0x10 (bit 0 enables the counter), the main counter at 0xF0.
fn hpet_read(off: u64) -> u64 {
    // SAFETY: the HPET's registers, through the direct map.
    unsafe { core::ptr::read_volatile((HPET.load(Ordering::Relaxed) + DIRECT_MAP + off) as *const u64) }
}

fn hpet_enable(addr: u64) -> Option<u64> {
    HPET.store(addr, Ordering::Relaxed);
    let period_fs = hpet_read(0) >> 32;
    if period_fs == 0 || period_fs > 100_000_000 {
        return None;
    }
    // SAFETY: as above; sets the enable bit.
    unsafe {
        let cfg = (addr + DIRECT_MAP + 0x10) as *mut u64;
        core::ptr::write_volatile(cfg, core::ptr::read_volatile(cfg) | 1);
    }
    Some(period_fs)
}

/// Counts TSC ticks over 10 ms of the HPET.
fn measure_with_hpet(period_fs: u64) -> u64 {
    let ticks_10ms = 10_000_000_000_000 / period_fs; // 10 ms in femtoseconds / period
    let h0 = hpet_read(0xF0);
    let t0 = cpu::rdtsc();
    while hpet_read(0xF0).wrapping_sub(h0) < ticks_10ms {}
    let t1 = cpu::rdtsc();
    let h1 = hpet_read(0xF0);
    // Scale to the exact HPET interval that passed.
    let elapsed_fs = (h1.wrapping_sub(h0)) as u128 * period_fs as u128;
    ((t1 - t0) as u128 * 1_000_000_000_000_000 / elapsed_fs) as u64
}

/// Counts TSC ticks over 10 ms of PIT channel 2 (1.193182 MHz).
fn measure_with_pit() -> u64 {
    let count: u16 = 11_932; // 10 ms
    cpu::outb(0x61, (cpu::inb(0x61) & !0x02) | 0x01); // gate on, speaker off
    cpu::outb(0x43, 0b1011_0000); // channel 2, lobyte/hibyte, mode 0
    cpu::outb(0x42, count as u8);
    cpu::outb(0x42, (count >> 8) as u8);
    let t0 = cpu::rdtsc();
    while cpu::inb(0x61) & 0x20 == 0 {}
    (cpu::rdtsc() - t0) * 100
}

/// Where the frequency came from, for the log.
pub struct Source {
    pub how: &'static str,
    pub check: Option<u64>,
}

pub fn init(hpet: Option<u64>, b0: u64) -> Source {
    let cpuid_hz = {
        let l15 = cpu::cpuid(0x15, 0);
        let max = cpu::cpuid(0, 0).eax;
        if max >= 0x15 && l15.eax != 0 && l15.ebx != 0 && l15.ecx != 0 {
            Some(l15.ecx as u64 * l15.ebx as u64 / l15.eax as u64)
        } else if max >= 0x16 && cpu::cpuid(0x16, 0).eax != 0 {
            Some(cpu::cpuid(0x16, 0).eax as u64 * 1_000_000)
        } else {
            None
        }
    };
    let hpet_hz = hpet.and_then(hpet_enable).map(measure_with_hpet);
    let (hz, how, check) = match (cpuid_hz, hpet_hz) {
        // Trust CPUID when the HPET agrees within 1 %; otherwise the measurement wins.
        (Some(c), Some(h)) if c.abs_diff(h) * 100 <= h => (c, "CPUID", Some(h)),
        (Some(_), Some(h)) => (h, "HPET (CPUID disagreed)", None),
        (Some(c), None) => (c, "CPUID", None),
        (None, Some(h)) => (h, "HPET", None),
        (None, None) => (measure_with_pit(), "PIT", None),
    };
    TSC_HZ.store(hz, Ordering::Relaxed);
    TSC_ZERO.store(cpu::rdtsc(), Ordering::Relaxed);
    B0.store(b0, Ordering::Relaxed);
    RTC_UNIX.store(crate::arch::rtc::read_unix().unwrap_or(0), Ordering::Relaxed);
    Source { how, check }
}

/// The RTC's reading at boot, in Unix seconds.
pub fn rtc_unix() -> u64 {
    RTC_UNIX.load(Ordering::Relaxed)
}

ktest!(time_monotonic_never_goes_backwards, {
    let mut last = now_ns();
    for _ in 0..1_000_000 {
        let t = now_ns();
        if t < last {
            return Err("the monotonic clock went backwards");
        }
        last = t;
    }
    Ok(())
});

ktest!(time_the_rtc_reads_a_plausible_date, {
    // 2026-01-01 .. 2100-01-01
    let s = rtc_unix();
    if (1_767_225_600..4_102_444_800).contains(&s) { Ok(()) } else { Err("the RTC's date isn't plausible") }
});

ktest!(time_tsc_frequency_is_known, {
    if tsc_hz() < 100_000_000 { Err("no plausible TSC frequency") } else { Ok(()) }
});
