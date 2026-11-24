// SPDX-License-Identifier: Apache-2.0
//! The timer interrupt (TASK-023): the local APIC in TSC-deadline mode. The
//! scheduler (TASK-026) arms it for each time slice and for sleepers.

use crate::arch::idt::Frame;
use crate::arch::{apic, cpu};
use core::sync::atomic::Ordering;

/// Each CPU counts its own timer's fires, and when it last did (its block, TASK-036).
pub fn interrupt(_f: &mut Frame) {
    let c = crate::smp::this();
    c.last_fire.store(cpu::rdtsc(), Ordering::SeqCst);
    c.fires.fetch_add(1, Ordering::SeqCst);
    // The timer is spent: nothing is armed now. (A one-shot timer can fire a
    // little before its deadline; the scheduler then arms it again, where it
    // would otherwise think the deadline still pending and arm nothing.)
    c.armed.store(0, Ordering::Relaxed);
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
        let before = crate::smp::this().fires.load(Ordering::SeqCst);
        let deadline = cpu::rdtsc() + crate::time::ns_to_ticks(1_000_000);
        apic::arm_deadline(deadline);
        let give_up = deadline + crate::time::ns_to_ticks(100_000_000);
        cpu::interrupts_on();
        while crate::smp::this().fires.load(Ordering::SeqCst) == before && cpu::rdtsc() < give_up {
            core::hint::spin_loop();
        }
        cpu::interrupts_off();
        if crate::smp::this().fires.load(Ordering::SeqCst) == before {
            return Err("the timer never fired");
        }
        *slot = crate::time::ticks_to_ns(crate::smp::this().last_fire.load(Ordering::SeqCst).saturating_sub(deadline));
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

/// Disarms this CPU's timer and lets through any fire already pending (a
/// deadline the scheduler armed that passed while interrupts were off), until
/// a quiet 20 µs: what the timer does next is the caller's own deadline.
fn quiet() {
    let c = crate::smp::this();
    loop {
        apic::arm_deadline(0);
        let f = c.fires.load(Ordering::SeqCst);
        cpu::interrupts_on();
        let until = cpu::rdtsc() + crate::time::ns_to_ticks(20_000);
        while cpu::rdtsc() < until {
            core::hint::spin_loop();
        }
        cpu::interrupts_off();
        if c.fires.load(Ordering::SeqCst) == f {
            return;
        }
    }
}

ktest!(timer_never_fires_early, {
    for _ in 0..20 {
        quiet();
        let before = crate::smp::this().fires.load(Ordering::SeqCst);
        let deadline = cpu::rdtsc() + crate::time::ns_to_ticks(200_000);
        // The scheduler leaves a later deadline of its own alone (`sched::arm`).
        crate::smp::this().armed.store(deadline, Ordering::Relaxed);
        apic::arm_deadline(deadline);
        cpu::interrupts_on();
        let give_up = deadline + crate::time::ns_to_ticks(1_000_000_000);
        while crate::smp::this().fires.load(Ordering::SeqCst) == before && cpu::rdtsc() < give_up {
            core::hint::spin_loop();
        }
        cpu::interrupts_off();
        if crate::smp::this().last_fire.load(Ordering::SeqCst) < deadline {
            return Err("the timer fired before its deadline");
        }
    }
    Ok(())
});
