// SPDX-License-Identifier: Apache-2.0
//! Time (TASK-023): the TSC is the clock, its frequency found from CPUID leaf
//! 0x15 (or 0x16), else measured against the HPET, else the PIT. The HPET also
//! checks a CPUID answer. The CMOS RTC gives the date once, at boot; UTC is that
//! plus the monotonic clock.
//!
//! - `MONOTONIC`: nanoseconds since the kernel's clock started; never backwards.
//! - `BOOT`: nanoseconds since the bootloader started (B0).
//! - `UTC`: nanoseconds since 1970, from the RTC at boot: the RTC's reading,
//!   less the zone's offset when the RTC keeps local time (`FreallyClock`,
//!   TASK-180A), plus the monotonic clock.
//! - `LOCAL`: `UTC` plus the zone's offset.

use crate::arch::cpu;
use core::sync::atomic::{AtomicU64, Ordering};
use fcore_abi::layout::DIRECT_MAP;

static TSC_HZ: AtomicU64 = AtomicU64::new(0);
static TSC_ZERO: AtomicU64 = AtomicU64::new(0);
static B0: AtomicU64 = AtomicU64::new(0);
static RTC_UNIX: AtomicU64 = AtomicU64::new(0);
/// The zone in use, packed: the offset in minutes (as u16) in the low half,
/// bit 16 set when the RTC keeps local time. Nothing kept: 0, the RTC read
/// as UTC.
static ZONE: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static HPET: AtomicU64 = AtomicU64::new(0);
/// Where the TSC's frequency came from (`fcore_abi::tsc_from`).
static TSC_FROM: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

pub fn tsc_hz() -> u64 {
    TSC_HZ.load(Ordering::Relaxed)
}

/// Where the TSC's frequency came from (Monitor → Boot says, TASK-073).
pub fn tsc_from() -> u8 {
    TSC_FROM.load(Ordering::Relaxed)
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
    utc_at(RTC_UNIX.load(Ordering::Relaxed), now_ns(), zone().rtc_ahead_s())
}

/// UTC from the RTC's reading at boot (seconds), the monotonic clock (ns)
/// and what the RTC is ahead of UTC by (seconds); never below 0.
fn utc_at(rtc_s: u64, mono_ns: u64, ahead_s: i64) -> u64 {
    let ns = rtc_s as i128 * 1_000_000_000 + mono_ns as i128 - ahead_s as i128 * 1_000_000_000;
    ns.clamp(0, u64::MAX as i128) as u64
}

/// Local time: UTC plus the zone's offset.
pub fn local_ns() -> u64 {
    let ns = utc_ns() as i128 + zone().offset_s() as i128 * 1_000_000_000;
    ns.clamp(0, u64::MAX as i128) as u64
}

/// The zone in use.
pub fn zone() -> fbootctl::clock::Zone {
    let v = ZONE.load(Ordering::Relaxed);
    fbootctl::clock::Zone { offset_min: v as u16 as i16, rtc_local: v & 1 << 16 != 0 }
}

/// Takes `z` from now on (a valid zone: `FreallyClock` decoded it).
pub fn set_zone(z: fbootctl::clock::Zone) {
    ZONE.store(z.offset_min as u16 as u32 | (z.rtc_local as u32) << 16, Ordering::Relaxed);
}

// The HPET: capabilities at 0x0 (period in femtoseconds in the top half, at
// 0x4), configuration at 0x10 (bit 0 enables the counter), the main counter
// at 0xF0. Read 32 bits at a time: every HPET answers those, and VirtualBox's
// (under Hyper-V) answered 64-bit reads of the counter wrongly now and then,
// so the TSC came out at 4 or 5 MHz (TASK-113). The low half wraps only
// every few minutes, far longer than a measurement.
fn hpet_read32(off: u64) -> u32 {
    // SAFETY: the HPET's registers, through the direct map.
    unsafe { core::ptr::read_volatile((HPET.load(Ordering::Relaxed) + DIRECT_MAP + off) as *const u32) }
}

/// No x86-64 CPU's TSC runs slower than this: a measurement below it is wrong.
const PLAUSIBLE_HZ: u64 = 100_000_000;

fn hpet_enable(addr: u64) -> Option<u64> {
    HPET.store(addr, Ordering::Relaxed);
    let period_fs = hpet_read32(4) as u64;
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
    let ticks_10ms = (10_000_000_000_000 / period_fs) as u32; // 10 ms in femtoseconds / period
    let h0 = hpet_read32(0xF0);
    let t0 = cpu::rdtsc();
    while hpet_read32(0xF0).wrapping_sub(h0) < ticks_10ms {}
    let t1 = cpu::rdtsc();
    let h1 = hpet_read32(0xF0);
    tsc_hz_from(t1 - t0, h1.wrapping_sub(h0), period_fs)
}

/// The TSC's frequency from `tsc_ticks` counted over `hpet_ticks` of an HPET
/// whose period is `period_fs` (scaled to the exact interval that passed).
fn tsc_hz_from(tsc_ticks: u64, hpet_ticks: u32, period_fs: u64) -> u64 {
    let elapsed_fs = (hpet_ticks as u128 * period_fs as u128).max(1);
    (tsc_ticks as u128 * 1_000_000_000_000_000 / elapsed_fs) as u64
}

/// The HPET's measurement, tried up to three times for a plausible one.
fn hpet_tsc_hz(period_fs: u64) -> Option<u64> {
    (0..3).map(|_| measure_with_hpet(period_fs)).find(|&h| h >= PLAUSIBLE_HZ)
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

/// A hypervisor's timing leaf, 0x40000010 (VirtualBox's *Minimal*
/// paravirtualization, VMware): the TSC's and the APIC bus's frequencies in
/// kHz. Its own word beats any measurement: its emulated HPET and PIT run on
/// a clock that lags and catches up, so 10 ms of them can pass in a few
/// microseconds of TSC (TASK-113).
pub fn hypervisor_timing() -> Option<(u64, u64)> {
    if cpu::cpuid(1, 0).ecx & (1 << 31) == 0 || cpu::cpuid(0x4000_0000, 0).eax < 0x4000_0010 {
        return None;
    }
    let t = cpu::cpuid(0x4000_0010, 0);
    (t.eax as u64 * 1000 >= PLAUSIBLE_HZ).then_some((t.eax as u64 * 1000, t.ebx as u64 * 1000))
}

pub fn init(hpet: Option<u64>, b0: u64) -> Source {
    if let Some((hz, _)) = hypervisor_timing() {
        return set(hz, "the hypervisor (CPUID 0x40000010)", fcore_abi::tsc_from::HYPERVISOR, None, b0);
    }
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
    let hpet_hz = hpet.and_then(hpet_enable).and_then(hpet_tsc_hz);
    let (hz, how, check) = match (cpuid_hz, hpet_hz) {
        // Trust CPUID when the HPET agrees within 1 %; otherwise the measurement wins.
        (Some(c), Some(h)) if c.abs_diff(h) * 100 <= h => (c, "CPUID", Some(h)),
        (Some(_), Some(h)) => (h, "HPET (CPUID disagreed)", None),
        (Some(c), None) => (c, "CPUID", None),
        (None, Some(h)) => (h, "HPET", None),
        (None, None) => (measure_with_pit(), "PIT", None),
    };
    let from = match (how, check) {
        ("CPUID", Some(_)) => fcore_abi::tsc_from::CPUID_HPET_AGREES,
        ("CPUID", None) => fcore_abi::tsc_from::CPUID,
        ("PIT", _) => fcore_abi::tsc_from::PIT,
        _ => fcore_abi::tsc_from::HPET,
    };
    set(hz, how, from, check, b0)
}

/// The clock started: the TSC's frequency, where it came from, the zero, B0, the RTC.
fn set(hz: u64, how: &'static str, from: u8, check: Option<u64>, b0: u64) -> Source {
    TSC_HZ.store(hz, Ordering::Relaxed);
    TSC_FROM.store(from, Ordering::Relaxed);
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

ktest!(time_utc_is_the_rtc_less_the_zone_when_it_keeps_local_time, {
    use fbootctl::clock::Zone;
    // The RTC read 12:00:00 on 2026-11-24, in Central time (UTC−06:00).
    let rtc = 1_795_521_600;
    let central = Zone { offset_min: -360, rtc_local: true };
    if utc_at(rtc, 5_000_000_000, central.rtc_ahead_s()) != (rtc + 6 * 3600 + 5) * 1_000_000_000 {
        return Err("an RTC on local time, six hours behind UTC: UTC isn't six hours on");
    }
    // The same zone, the RTC on UTC: its reading is UTC already.
    if utc_at(rtc, 0, Zone { rtc_local: false, ..central }.rtc_ahead_s()) != rtc * 1_000_000_000 {
        return Err("an RTC on UTC was moved by the zone");
    }
    // East of UTC (India, +05:30), the RTC on local time.
    if utc_at(rtc, 0, Zone { offset_min: 330, rtc_local: true }.rtc_ahead_s()) != (rtc - 19_800) * 1_000_000_000 {
        return Err("an RTC on local time east of UTC");
    }
    // Set, read back, the local clock its offset from UTC; then put back.
    let was = zone();
    set_zone(central);
    let (z, u, l) = (zone(), utc_ns(), local_ns());
    set_zone(was);
    if z != central {
        return Err("the zone set isn't the zone read");
    }
    if (u as i128 - l as i128 - 6 * 3_600_000_000_000).abs() > 1_000_000_000 {
        return Err("LOCAL isn't UTC less six hours in Central time");
    }
    Ok(())
});

ktest!(time_a_tsc_measured_against_the_hpet, {
    // 14.31818 MHz (69,841,279 fs): 143,182 ticks is 10 ms; 24,000,000 TSC
    // ticks over it is 2.4 GHz, whatever the counter's low half wrapped through.
    let hz = tsc_hz_from(24_000_000, 143_182, 69_841_279);
    if hz.abs_diff(2_400_000_000) > 2_400_000 {
        return Err("10 ms of a 14.3 MHz HPET against 24 M TSC ticks isn't 2.4 GHz");
    }
    let h0 = u32::MAX - 100;
    if tsc_hz_from(24_000_000, h0.wrapping_add(143_182).wrapping_sub(h0), 69_841_279) != hz {
        return Err("the counter's wrap changed the answer");
    }
    // No HPET time passed (the measurement's loop never ends so, but the
    // function alone may be asked): an answer, never a division by zero.
    let _ = tsc_hz_from(1, 0, 69_841_279);
    Ok(())
});

ktest!(time_tsc_frequency_is_known, {
    if tsc_hz() < 100_000_000 { Err("no plausible TSC frequency") } else { Ok(()) }
});
