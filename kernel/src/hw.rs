// SPDX-License-Identifier: Apache-2.0
//! Hardware access (TASK-040): resources, I/O ports, MMIO, interrupts.
//!
//! - A **resource** is a range a holder may use: the root (conductor hands it
//!   to devmgr) covers everything; `resource_create` carves a narrower one of
//!   one kind (MMIO, I/O ports, IRQs) from a parent that covers it.
//! - **I/O ports:** `ioport_grant` adds a covered range to the process's list;
//!   the TSS's I/O bitmap opens exactly those ports while one of its threads
//!   runs, and closes them when another process gets the CPU.
//! - **MMIO:** `mmio_map` maps a covered physical range into the caller, never
//!   executable, never over RAM the kernel hands out or holds itself.
//! - **Interrupts:** `interrupt_bind` gives a covered GSI its own vector and an
//!   object that's signalled when it fires; the line stays masked until
//!   `interrupt_ack`.

use crate::obj::{Header, Object, ObjectRef, Resource, ResourceKind, signal};
use crate::sync::SpinLock;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use fcore_abi::{FErr, MemKind};

/// Whether `r` covers `[base, base + len)` of `kind`.
pub fn covers(r: &Resource, kind: ResourceKind, base: u64, len: u64) -> bool {
    let Some(end) = base.checked_add(len) else { return false };
    len != 0
        && (r.kind == ResourceKind::Root || (r.kind == kind && base >= r.base && end <= r.base.saturating_add(r.len)))
}

pub fn resource_of(o: &ObjectRef) -> Result<Resource, FErr> {
    match &**o {
        Object::Resource(r) => Ok(*r),
        _ => Err(FErr::WrongType),
    }
}

/// A narrower resource carved from `parent`.
pub fn create(parent: &Resource, kind: ResourceKind, base: u64, len: u64) -> Result<Resource, FErr> {
    if kind == ResourceKind::Root {
        return Err(FErr::InvalidArgs);
    }
    if !covers(parent, kind, base, len) {
        return Err(FErr::AccessDenied);
    }
    if kind == ResourceKind::IoPort && base.saturating_add(len) > 0x1_0000 {
        return Err(FErr::InvalidArgs);
    }
    Ok(Resource { kind, base, len })
}

// ── I/O ports ──────────────────────────────────────────────────────────────

/// Called by the scheduler before `pid`'s thread runs (`ranges` its ports).
pub fn io_switch(pid: u32, ranges: &[(u16, u16)]) {
    // This CPU's TSS (each CPU has its own bitmap, TASK-036). Interrupts off.
    let c = crate::smp::this();
    let mut open = c.io_open.lock();
    if c.io_owner.load(Ordering::Relaxed) == pid && open.as_slice() == ranges {
        return;
    }
    for &(p, n) in open.iter() {
        crate::arch::gdt::io_set(p, n, false);
    }
    open.clear();
    for &(p, n) in ranges {
        crate::arch::gdt::io_set(p, n, true);
        open.push((p, n));
    }
    c.io_owner.store(pid, Ordering::Relaxed);
}

/// Opens `[port, port + len)` for the calling process (already checked).
pub fn grant_ports(pid: u32, list: &SpinLock<Vec<(u16, u16)>>, port: u16, len: u16) {
    let ranges = {
        let mut l = list.lock();
        if !l.contains(&(port, len)) {
            l.push((port, len));
        }
        l.clone()
    };
    // Reapply now, on the CPU the caller runs on (its other CPUs at their
    // next switch to it: the ranges differ).
    let _irq = crate::arch::cpu::IrqOff::new();
    io_switch(pid, &ranges);
}

// ── MMIO ───────────────────────────────────────────────────────────────────

/// Whether `[phys, phys + len)` touches memory the kernel hands out or holds.
pub fn is_kernel_memory(phys: u64, len: u64) -> bool {
    let Some(bi) = crate::boot_info() else { return true };
    // SAFETY: BootInfo's memory map lives in BOOT_DATA pages, kept forever.
    let map = unsafe { core::slice::from_raw_parts(bi.memory_map.ptr, bi.memory_map.len as usize) };
    let end = phys.saturating_add(len);
    map.iter().any(|r| {
        matches!(
            r.kind,
            MemKind::Usable | MemKind::Image | MemKind::BootInfo | MemKind::RuntimeCode | MemKind::RuntimeData
        ) && phys < r.base + r.len
            && r.base < end
    })
}

// ── Interrupts ─────────────────────────────────────────────────────────────

pub struct IrqLine {
    pub header: Header,
    pub gsi: u32,
    pub vector: u8,
    /// A message-signalled line (MSI-X, TASK-092A): no IOAPIC pin to mask;
    /// the device writes the vector, edge-triggered.
    pub msi: bool,
}

impl Drop for IrqLine {
    fn drop(&mut self) {
        if !self.msi {
            crate::arch::ioapic::mask(self.gsi);
        }
        LINES.lock()[self.vector as usize] = None;
    }
}

const FIRST_VECTOR: u8 = 48;
const LAST_VECTOR: u8 = 239;
static LINES: SpinLock<[Option<alloc::sync::Weak<IrqLine>>; 256]> = SpinLock::new([const { None }; 256]);

fn irq_handler(f: &mut crate::arch::idt::Frame) {
    let line = LINES.lock()[f.vector as usize].as_ref().and_then(alloc::sync::Weak::upgrade);
    if let Some(l) = line {
        // Masked until the driver acks: a level-triggered line would fire again at once.
        if !l.msi {
            crate::arch::ioapic::mask(l.gsi);
        }
        l.header.update(0, signal::SIGNALED);
    }
    crate::arch::apic::eoi();
}

/// Binds `gsi` (MADT override `flags`: polarity and trigger) to a new line.
pub fn bind(gsi: u32, flags: u16) -> Result<Arc<IrqLine>, FErr> {
    let mut lines = LINES.lock();
    if lines.iter().flatten().any(|w| w.upgrade().is_some_and(|l| l.gsi == gsi)) {
        return Err(FErr::AlreadyExists);
    }
    let vector = (FIRST_VECTOR..=LAST_VECTOR)
        .find(|v| lines[*v as usize].as_ref().is_none_or(|w| w.upgrade().is_none()))
        .ok_or(FErr::NoMemory)?;
    let line = Arc::new(IrqLine { header: Header::new(), gsi, vector, msi: false });
    lines[vector as usize] = Some(Arc::downgrade(&line));
    drop(lines);
    crate::exceptions::route(vector, irq_handler);
    crate::arch::ioapic::route(gsi, vector, crate::arch::apic::id(), flags).map_err(|_| FErr::InvalidArgs)?;
    Ok(line)
}

/// The driver is done with the last interrupt: clear the signal and unmask.
pub fn ack(l: &IrqLine) {
    l.header.update(signal::SIGNALED, 0);
    if !l.msi {
        crate::arch::ioapic::unmask(l.gsi);
    }
}

/// A vector for a device's MSI-X entry (TASK-092A): the line, and the
/// message its table entry takes (the address: this CPU's local APIC,
/// physical destination; the data: the vector, fixed delivery, edge).
pub fn bind_msi() -> Result<(Arc<IrqLine>, u32, u32), FErr> {
    let dest = crate::arch::apic::id();
    // Past 255 an x2APIC destination needs interrupt remapping (not yet).
    if dest > 255 {
        return Err(FErr::NotSupported);
    }
    let mut lines = LINES.lock();
    let vector = (FIRST_VECTOR..=LAST_VECTOR)
        .find(|v| lines[*v as usize].as_ref().is_none_or(|w| w.upgrade().is_none()))
        .ok_or(FErr::NoMemory)?;
    let line = Arc::new(IrqLine { header: Header::new(), gsi: u32::MAX, vector, msi: true });
    lines[vector as usize] = Some(Arc::downgrade(&line));
    drop(lines);
    crate::exceptions::route(vector, irq_handler);
    Ok((line, 0xFEE0_0000 | (dest << 12), vector as u32))
}

ktest!(msi_a_bound_vector_fires_its_line, {
    let (line, address, data) = bind_msi().map_err(|_| "bind_msi refused")?;
    if address & 0xFFF0_0000 != 0xFEE0_0000 || !(FIRST_VECTOR as u32..=LAST_VECTOR as u32).contains(&data) {
        return Err("the message isn't a local APIC's");
    }
    // What the device would write: the vector, to this CPU.
    crate::arch::apic::send_ipi(crate::arch::apic::id(), data as u8);
    let give_up = crate::time::now_ns() + 100_000_000;
    crate::arch::cpu::interrupts_on();
    while line.header.get() & signal::SIGNALED == 0 && crate::time::now_ns() < give_up {
        core::hint::spin_loop();
    }
    crate::arch::cpu::interrupts_off();
    if line.header.get() & signal::SIGNALED == 0 {
        return Err("the vector never signalled its line");
    }
    ack(&line);
    if line.header.get() & signal::SIGNALED != 0 {
        return Err("ack didn't clear it");
    }
    Ok(())
});
