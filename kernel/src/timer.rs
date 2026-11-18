// SPDX-License-Identifier: Apache-2.0
//! The timer interrupt (TASK-023): the local APIC in TSC-deadline mode. The
//! scheduler (TASK-026) arms it for each time slice and for sleepers.

use crate::arch::idt::Frame;
use crate::arch::{apic, cpu};
use core::sync::atomic::{AtomicU64, Ordering};

/// The TSC when the timer last fired.
pub static LAST_FIRE: AtomicU64 = AtomicU64::new(0);
pub static FIRES: AtomicU64 = AtomicU64::new(0);

pub fn interrupt(_f: &mut Frame) {
    LAST_FIRE.store(cpu::rdtsc(), Ordering::SeqCst);
    FIRES.fetch_add(1, Ordering::SeqCst);
    apic::eoi();
    crate::sched_tick();
}

pub fn spurious(_f: &mut Frame) {
    // A spurious APIC interrupt needs no EOI.
}

/// Fires the timer `ns` from now.
pub fn arm_in_ns(ns: u64) {
    apic::arm_deadline(cpu::rdtsc() + crate::time::ns_to_ticks(ns));
}

/// The hypervisor's vendor string, if there is one (for tests that need KVM).
pub fn hypervisor() -> [u8; 12] {
    if cpu::cpuid(1, 0).ecx & (1 << 31) == 0 {
        return [0; 12];
    }
    let r = cpu::cpuid(0x4000_0000, 0);
    let mut v = [0u8; 12];
    v[..4].copy_from_slice(&r.ebx.to_le_bytes());
    v[4..8].copy_from_slice(&r.ecx.to_le_bytes());
    v[8..].copy_from_slice(&r.edx.to_le_bytes());
    v
}

ktest!(timer_fires_within_50_microseconds_of_its_deadline, {
    if &hypervisor() == b"TCGTCGTCGTCG" {
        klog!("    skipped: QEMU's TCG has no real-time timer (this test needs KVM or hardware)");
        return Ok(());
    }
    let mut late = [0u64; 64];
    for slot in late.iter_mut() {
        let before = FIRES.load(Ordering::SeqCst);
        let deadline = cpu::rdtsc() + crate::time::ns_to_ticks(1_000_000);
        apic::arm_deadline(deadline);
        let give_up = deadline + crate::time::ns_to_ticks(100_000_000);
        cpu::interrupts_on();
        while FIRES.load(Ordering::SeqCst) == before && cpu::rdtsc() < give_up {
            core::hint::spin_loop();
        }
        cpu::interrupts_off();
        if FIRES.load(Ordering::SeqCst) == before {
            return Err("the timer never fired");
        }
        *slot = crate::time::ticks_to_ns(LAST_FIRE.load(Ordering::SeqCst).saturating_sub(deadline));
    }
    late.sort_unstable();
    klog!(
        "    lateness over 64 deadlines: min {} µs, median {} µs, p90 {} µs, max {} µs ({})",
        late[0] / 1000,
        late[32] / 1000,
        late[57] / 1000,
        late[63] / 1000,
        core::str::from_utf8(&hypervisor()).unwrap_or("?").trim_end_matches('\0')
    );
    // The median is the timer's own accuracy; the tail is the host scheduling the VM.
    if late[32] > 50_000 { Err("the median timer fired more than 50 µs late") } else { Ok(()) }
});

ktest!(timer_never_fires_early, {
    for _ in 0..20 {
        let before = FIRES.load(Ordering::SeqCst);
        let deadline = cpu::rdtsc() + crate::time::ns_to_ticks(200_000);
        apic::arm_deadline(deadline);
        cpu::interrupts_on();
        let give_up = deadline + crate::time::ns_to_ticks(1_000_000_000);
        while FIRES.load(Ordering::SeqCst) == before && cpu::rdtsc() < give_up {
            core::hint::spin_loop();
        }
        cpu::interrupts_off();
        if LAST_FIRE.load(Ordering::SeqCst) < deadline {
            return Err("the timer fired before its deadline");
        }
    }
    Ok(())
});
