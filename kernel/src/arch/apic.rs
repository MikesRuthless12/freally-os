// SPDX-License-Identifier: Apache-2.0
//! The local APIC (TASK-023), in x2APIC mode when the CPU has it (MSRs), else
//! xAPIC (memory-mapped through the direct map), and the legacy 8259 PICs,
//! remapped out of the way and fully masked.
//!
//! The timer is armed with an absolute TSC value. It runs in TSC-deadline
//! mode when the CPU has it; without it (VirtualBox shows a guest none,
//! TASK-113) in one-shot mode, each deadline turned into a count of the APIC
//! timer's clock, measured against the TSC at boot ([`calibrate`]).

use super::cpu;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use fcore_abi::layout::DIRECT_MAP;

pub const TIMER_VECTOR: u8 = 32;
/// Another CPU asks this one to reschedule (TASK-036).
pub const RESCHED_VECTOR: u8 = 33;
pub const SPURIOUS_VECTOR: u8 = 255;

const IA32_APIC_BASE: u32 = 0x1B;
const IA32_TSC_DEADLINE: u32 = 0x6E0;

const REG_ID: u32 = 0x20;
const REG_TPR: u32 = 0x80;
const REG_EOI: u32 = 0xB0;
const REG_SVR: u32 = 0xF0;
const REG_ICR: u32 = 0x300;
const REG_ICR_HIGH: u32 = 0x310;
const REG_LVT_TIMER: u32 = 0x320;
const REG_TIMER_INIT: u32 = 0x380;
const REG_TIMER_CUR: u32 = 0x390;
const REG_TIMER_DIV: u32 = 0x3E0;
/// The one-shot timer's divider: 16 (`0b0011`), so a 32-bit count spans
/// over a minute at a 1 GHz bus clock.
const DIV_16: u32 = 0b0011;

static X2APIC: AtomicBool = AtomicBool::new(false);
static XAPIC_BASE: AtomicU64 = AtomicU64::new(0);
/// The one-shot timer's counts a second (after the divider); 0 in TSC-deadline mode.
static ONESHOT_HZ: AtomicU64 = AtomicU64::new(0);

fn read(reg: u32) -> u32 {
    if X2APIC.load(Ordering::Relaxed) {
        // SAFETY: x2APIC MSRs 0x800 + reg/16 exist once x2APIC mode is on.
        unsafe { cpu::rdmsr(0x800 + (reg >> 4)) as u32 }
    } else {
        // SAFETY: the xAPIC page, through the direct map (UC by the MTRRs).
        unsafe {
            core::ptr::read_volatile((XAPIC_BASE.load(Ordering::Relaxed) + DIRECT_MAP + reg as u64) as *const u32)
        }
    }
}

fn write(reg: u32, v: u32) {
    if X2APIC.load(Ordering::Relaxed) {
        // SAFETY: as above.
        unsafe { cpu::wrmsr(0x800 + (reg >> 4), v as u64) };
    } else {
        // SAFETY: as above.
        unsafe {
            core::ptr::write_volatile((XAPIC_BASE.load(Ordering::Relaxed) + DIRECT_MAP + reg as u64) as *mut u32, v)
        };
    }
}

/// Masks both 8259s after moving them to vectors 32–47 (so a stray IRQ can't
/// look like an exception).
fn mask_pics() {
    for (port, v) in [(0x20, 0x11), (0xA0, 0x11), (0x21, 32), (0xA1, 40), (0x21, 4), (0xA1, 2), (0x21, 1), (0xA1, 1)] {
        cpu::outb(port, v);
    }
    cpu::outb(0x21, 0xFF);
    cpu::outb(0xA1, 0xFF);
}

pub fn x2apic() -> bool {
    X2APIC.load(Ordering::Relaxed)
}

pub fn tsc_deadline_supported() -> bool {
    cpu::cpuid(1, 0).ecx & (1 << 24) != 0
}

/// Turns on the boot CPU's local APIC; returns its ID.
pub fn init(lapic_addr: u64, pics: bool) -> u32 {
    if pics {
        mask_pics();
    }
    // SAFETY: IA32_APIC_BASE exists on every x86-64 CPU.
    let base = unsafe { cpu::rdmsr(IA32_APIC_BASE) };
    if cpu::cpuid(1, 0).ecx & (1 << 21) != 0 {
        X2APIC.store(true, Ordering::Relaxed);
    } else {
        XAPIC_BASE.store(if lapic_addr != 0 { lapic_addr } else { base & 0xF_FFFF_F000 }, Ordering::Relaxed);
    }
    init_cpu()
}

/// Turns on this CPU's local APIC, in the boot CPU's mode; returns its ID.
pub fn init_cpu() -> u32 {
    // SAFETY: IA32_APIC_BASE exists on every x86-64 CPU.
    let base = unsafe { cpu::rdmsr(IA32_APIC_BASE) };
    if x2apic() {
        // SAFETY: enabling x2APIC mode (bit 10) together with the APIC (bit 11).
        unsafe { cpu::wrmsr(IA32_APIC_BASE, base | (1 << 11) | (1 << 10)) };
    } else {
        // SAFETY: the APIC enable bit.
        unsafe { cpu::wrmsr(IA32_APIC_BASE, base | (1 << 11)) };
    }
    write(REG_TPR, 0);
    write(REG_SVR, 0x100 | SPURIOUS_VECTOR as u32); // enabled, spurious vector 255
    if tsc_deadline_supported() {
        write(REG_LVT_TIMER, TIMER_VECTOR as u32 | (2 << 17));
    } else {
        // One-shot (mode 00), stopped until a deadline is armed.
        write(REG_TIMER_DIV, DIV_16);
        write(REG_LVT_TIMER, TIMER_VECTOR as u32);
        write(REG_TIMER_INIT, 0);
    }
    id()
}

/// Without TSC-deadline, measures the one-shot timer's clock against the
/// TSC over 10 ms (the TSC's frequency known by now); its counts a second,
/// or `None` in TSC-deadline mode. Interrupts are off; every CPU's APIC
/// timer runs from the same clock, so the boot CPU measures it once.
pub fn calibrate() -> Option<u64> {
    if tsc_deadline_supported() {
        return None;
    }
    // A hypervisor that says its APIC bus's frequency is believed: its
    // emulated timers can't be measured against (`time::hypervisor_timing`).
    if let Some((_, bus_hz)) = crate::time::hypervisor_timing().filter(|t| t.1 > 0) {
        let hz = bus_hz / 16;
        ONESHOT_HZ.store(hz.max(1), Ordering::Relaxed);
        return Some(hz);
    }
    write(REG_TIMER_INIT, u32::MAX);
    let t0 = cpu::rdtsc();
    let span = crate::time::ns_to_ticks(10_000_000);
    while cpu::rdtsc().wrapping_sub(t0) < span {}
    let counted = (u32::MAX - read(REG_TIMER_CUR)) as u128;
    let ticks = cpu::rdtsc().wrapping_sub(t0) as u128;
    write(REG_TIMER_INIT, 0);
    let hz = (counted * crate::time::tsc_hz() as u128 / ticks.max(1)) as u64;
    ONESHOT_HZ.store(hz.max(1), Ordering::Relaxed);
    Some(hz)
}

/// The one-shot count for `ticks` TSC ticks from now: at least 1 (0 would
/// stop the timer), at most a full count (a later deadline fires early, and
/// the scheduler arms it again).
fn oneshot_count(ticks: u64, oneshot_hz: u64, tsc_hz: u64) -> u32 {
    // Rounded up: a count never ends before the deadline it stands for.
    let n = (ticks as u128 * oneshot_hz as u128).div_ceil(tsc_hz.max(1) as u128);
    n.clamp(1, u32::MAX as u128) as u32
}

pub fn id() -> u32 {
    let v = read(REG_ID);
    if x2apic() { v } else { v >> 24 }
}

pub fn eoi() {
    write(REG_EOI, 0);
}

/// Arms the timer to fire at the absolute TSC value `tsc` (0 disarms it).
pub fn arm_deadline(tsc: u64) {
    let oneshot_hz = ONESHOT_HZ.load(Ordering::Relaxed);
    if oneshot_hz == 0 {
        // SAFETY: IA32_TSC_DEADLINE exists when CPUID says TSC-deadline is
        // supported (and without it, `calibrate` has set the one-shot clock).
        unsafe { cpu::wrmsr(IA32_TSC_DEADLINE, tsc) };
        return;
    }
    let count = match tsc {
        0 => 0,
        t => oneshot_count(t.saturating_sub(cpu::rdtsc()), oneshot_hz, crate::time::tsc_hz()),
    };
    write(REG_TIMER_INIT, count);
}

ktest!(apic_a_oneshot_count_is_never_zero_nor_past_the_register, {
    // 1 ms at a 3 GHz TSC and a 62.5 MHz (1 GHz / 16) timer clock: 62,500 counts.
    if oneshot_count(3_000_000, 62_500_000, 3_000_000_000) != 62_500 {
        return Err("1 ms isn't 62,500 counts");
    }
    if oneshot_count(0, 62_500_000, 3_000_000_000) != 1 {
        return Err("a deadline already passed must still fire (a count of 1, not 0)");
    }
    if oneshot_count(u64::MAX, 62_500_000, 3_000_000_000) != u32::MAX {
        return Err("a far deadline must clamp to a full count");
    }
    Ok(())
});

/// Sends interrupt `vector` to the CPU with APIC ID `dest` (fixed, edge).
pub fn send_ipi(dest: u32, vector: u8) {
    icr(dest, vector as u32);
}

/// Sends an NMI to the CPU with APIC ID `dest` (TLB shootdowns, the panic stop).
pub fn send_nmi(dest: u32) {
    icr(dest, 0x4400); // NMI, assert
}

/// INIT to an AP: it waits for a SIPI (TASK-036).
pub fn send_init(dest: u32) {
    icr(dest, 0x4500); // INIT, assert
}

/// A startup IPI: the AP starts in real mode at `page` × 4 KiB (below 1 MiB).
pub fn send_sipi(dest: u32, page: u8) {
    icr(dest, 0x4600 | page as u32); // STARTUP, assert
}

/// Writes the ICR, with interrupts off (an interrupt between the xAPIC's two
/// writes could send its own IPI in between), waiting for the xAPIC to take it.
fn icr(dest: u32, low: u32) {
    let _irq = cpu::IrqOff::new();
    if x2apic() {
        // SAFETY: the x2APIC ICR is one 64-bit MSR.
        unsafe { cpu::wrmsr(0x830, ((dest as u64) << 32) | low as u64) };
    } else {
        while read(REG_ICR) & (1 << 12) != 0 {
            core::hint::spin_loop();
        }
        write(REG_ICR_HIGH, dest << 24);
        write(REG_ICR, low);
        while read(REG_ICR) & (1 << 12) != 0 {
            core::hint::spin_loop();
        }
    }
}
