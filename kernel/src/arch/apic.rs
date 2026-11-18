// SPDX-License-Identifier: Apache-2.0
//! The local APIC (TASK-023), in x2APIC mode when the CPU has it (MSRs), else
//! xAPIC (memory-mapped through the direct map), and the legacy 8259 PICs,
//! remapped out of the way and fully masked.
//!
//! The timer runs in TSC-deadline mode: arm it with an absolute TSC value.

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

static X2APIC: AtomicBool = AtomicBool::new(false);
static XAPIC_BASE: AtomicU64 = AtomicU64::new(0);

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
    let lvt = if tsc_deadline_supported() { TIMER_VECTOR as u32 | (2 << 17) } else { TIMER_VECTOR as u32 | (1 << 16) };
    write(REG_LVT_TIMER, lvt);
    id()
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
    // SAFETY: IA32_TSC_DEADLINE exists when CPUID says TSC-deadline is supported.
    unsafe { cpu::wrmsr(IA32_TSC_DEADLINE, tsc) };
}

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
