// SPDX-License-Identifier: Apache-2.0
//! I/O APICs (TASK-023): every redirection entry masked at boot; `route` wires
//! a GSI to a vector on a CPU (drivers ask through `interrupt_bind`, TASK-040),
//! honouring the MADT's polarity and trigger overrides.

use crate::acpi_early::{IoApic, Madt};
use crate::sync::SpinLock;
use fcore_abi::layout::DIRECT_MAP;

static IOAPICS: SpinLock<([IoApic; crate::acpi_early::MAX_IOAPICS], usize)> =
    SpinLock::new(([IoApic { id: 0, addr: 0, gsi_base: 0 }; crate::acpi_early::MAX_IOAPICS], 0));

fn reg(io: &IoApic, r: u32) -> u32 {
    // SAFETY: IOREGSEL at +0, IOWIN at +0x10, through the direct map (UC by the MTRRs).
    unsafe {
        core::ptr::write_volatile((io.addr + DIRECT_MAP) as *mut u32, r);
        core::ptr::read_volatile((io.addr + DIRECT_MAP + 0x10) as *const u32)
    }
}

fn set(io: &IoApic, r: u32, v: u32) {
    // SAFETY: as above.
    unsafe {
        core::ptr::write_volatile((io.addr + DIRECT_MAP) as *mut u32, r);
        core::ptr::write_volatile((io.addr + DIRECT_MAP + 0x10) as *mut u32, v);
    }
}

/// How many inputs an I/O APIC has.
pub fn inputs(io: &IoApic) -> u32 {
    ((reg(io, 1) >> 16) & 0xFF) + 1
}

/// Masks every input of every I/O APIC.
pub fn init(madt: &Madt) {
    let mut g = IOAPICS.lock();
    for (i, io) in madt.ioapics[..madt.ioapic_count].iter().enumerate() {
        for n in 0..inputs(io) {
            set(io, 0x10 + 2 * n, 1 << 16);
            set(io, 0x11 + 2 * n, 0);
        }
        g.0[i] = *io;
    }
    g.1 = madt.ioapic_count;
}

/// Wires `gsi` to `vector` on the CPU with APIC ID `dest`. `flags` are the MADT
/// override flags (polarity bits 0–1: 3 = active low; trigger bits 2–3: 3 = level).
pub fn route(gsi: u32, vector: u8, dest: u32, flags: u16) -> Result<(), &'static str> {
    let g = IOAPICS.lock();
    let io = g.0[..g.1]
        .iter()
        .find(|io| gsi >= io.gsi_base && gsi < io.gsi_base + inputs(io))
        .ok_or("no I/O APIC has that GSI")?;
    let n = gsi - io.gsi_base;
    let mut low = vector as u32;
    if flags & 0b11 == 0b11 {
        low |= 1 << 13; // active low
    }
    if (flags >> 2) & 0b11 == 0b11 {
        low |= 1 << 15; // level triggered
    }
    set(io, 0x11 + 2 * n, dest << 24);
    set(io, 0x10 + 2 * n, low);
    Ok(())
}

/// Masks `gsi` again.
pub fn mask(gsi: u32) {
    let g = IOAPICS.lock();
    if let Some(io) = g.0[..g.1].iter().find(|io| gsi >= io.gsi_base && gsi < io.gsi_base + inputs(io)) {
        let n = gsi - io.gsi_base;
        let low = reg(io, 0x10 + 2 * n);
        set(io, 0x10 + 2 * n, low | (1 << 16));
    }
}
