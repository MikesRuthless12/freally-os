// SPDX-License-Identifier: Apache-2.0
//! facpi: reads the firmware's ACPI tables (no_std, no allocation beyond a
//! table list): the RSDP, the XSDT (or RSDT), and the tables devmgr uses:
//! MADT, FADT, HPET, MCFG and FPDT (and the FBPT it points to).
//!
//! Everything is read through `Mem`, so the same code runs on a test's byte
//! buffer and on devmgr's read-only mappings. Every checksum and length is
//! checked; a table that's wrong is skipped, never trusted.
#![no_std]

extern crate alloc;

use alloc::vec::Vec;

/// Physical memory, as the reader sees it.
pub trait Mem {
    /// Copies `buf.len()` bytes from physical `addr`; false if it can't.
    fn read(&mut self, addr: u64, buf: &mut [u8]) -> bool;
}

fn u16_of(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_of(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u64_of(b: &[u8], o: usize) -> u64 {
    let mut x = [0u8; 8];
    x.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(x)
}

fn sums_to_zero(b: &[u8]) -> bool {
    b.iter().fold(0u8, |a, &x| a.wrapping_add(x)) == 0
}

/// The largest table read whole (the DSDT can be large; nothing here needs it).
const MAX_TABLE: usize = 64 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TableRef {
    pub signature: [u8; 4],
    pub addr: u64,
    pub len: u32,
}

/// Reads a whole table at `addr`, checking its length and checksum.
pub fn table(m: &mut impl Mem, addr: u64) -> Option<Vec<u8>> {
    let mut head = [0u8; 36];
    if !m.read(addr, &mut head) {
        return None;
    }
    let len = u32_of(&head, 4) as usize;
    if !(36..=MAX_TABLE).contains(&len) {
        return None;
    }
    let mut t = alloc::vec![0u8; len];
    if !m.read(addr, &mut t) || !sums_to_zero(&t) {
        return None;
    }
    Some(t)
}

/// Every table the root table lists (XSDT if the RSDP has one, else RSDT).
pub fn tables(m: &mut impl Mem, rsdp: u64) -> Option<Vec<TableRef>> {
    let mut r = [0u8; 36];
    if !m.read(rsdp, &mut r[..20]) || &r[..8] != b"RSD PTR " || !sums_to_zero(&r[..20]) {
        return None;
    }
    let (root, wide) = if r[15] >= 2 && m.read(rsdp, &mut r) && sums_to_zero(&r[..36]) && u64_of(&r, 24) != 0 {
        (u64_of(&r, 24), true)
    } else {
        (u32_of(&r, 16) as u64, false)
    };
    let rt = table(m, root)?;
    if &rt[..4] != if wide { b"XSDT" } else { b"RSDT" } {
        return None;
    }
    let step = if wide { 8 } else { 4 };
    let mut out = Vec::new();
    for o in (36..rt.len()).step_by(step) {
        if o + step > rt.len() {
            break;
        }
        let addr = if wide { u64_of(&rt, o) } else { u32_of(&rt, o) as u64 };
        let mut h = [0u8; 8];
        if addr != 0 && m.read(addr, &mut h) {
            out.push(TableRef { signature: [h[0], h[1], h[2], h[3]], addr, len: u32_of(&h, 4) });
        }
    }
    Some(out)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Madt {
    pub lapic_addr: u64,
    pub cpus: u32,
    pub ioapics: u32,
    pub overrides: u32,
    pub has_8259: bool,
}

pub fn madt(t: &[u8]) -> Option<Madt> {
    if t.len() < 44 || &t[..4] != b"APIC" {
        return None;
    }
    let mut m = Madt { lapic_addr: u32_of(t, 36) as u64, has_8259: u32_of(t, 40) & 1 != 0, ..Madt::default() };
    let mut o = 44;
    while o + 2 <= t.len() {
        let (kind, len) = (t[o], t[o + 1] as usize);
        if len < 2 || o + len > t.len() {
            break;
        }
        match kind {
            0 if len >= 8 && u32_of(t, o + 4) & 3 != 0 => m.cpus += 1,
            9 if len >= 16 && u32_of(t, o + 8) & 3 != 0 => m.cpus += 1,
            1 => m.ioapics += 1,
            2 => m.overrides += 1,
            5 if len >= 12 => m.lapic_addr = u64_of(t, o + 4),
            _ => {}
        }
        o += len;
    }
    Some(m)
}

/// An ISA IRQ's GSI and MADT flags (polarity, trigger): the identity unless an
/// interrupt source override says otherwise.
pub fn isa_gsi(t: &[u8], irq: u8) -> (u32, u16) {
    if t.len() >= 44 && &t[..4] == b"APIC" {
        let mut o = 44;
        while o + 2 <= t.len() {
            let (kind, len) = (t[o], t[o + 1] as usize);
            if len < 2 || o + len > t.len() {
                break;
            }
            if kind == 2 && len >= 10 && t[o + 3] == irq {
                return (u32_of(t, o + 4), u16_of(t, o + 8));
            }
            o += len;
        }
    }
    (irq as u32, 0)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Fadt {
    pub dsdt: u64,
    /// IAPC_BOOT_ARCH bit 1: a PS/2 controller (8042) is present.
    pub has_8042: bool,
    pub pm_timer_port: u32,
}

pub fn fadt(t: &[u8]) -> Option<Fadt> {
    if t.len() < 116 || &t[..4] != b"FACP" {
        return None;
    }
    let x_dsdt = if t.len() >= 148 { u64_of(t, 140) } else { 0 };
    Some(Fadt {
        dsdt: if x_dsdt != 0 { x_dsdt } else { u32_of(t, 40) as u64 },
        // An ACPI 1.0 FADT has no boot-architecture flags: assume the 8042 is there.
        has_8042: t[8] < 2 || u16_of(t, 109) & 2 != 0,
        pm_timer_port: u32_of(t, 76),
    })
}

/// The HPET's register block address.
pub fn hpet(t: &[u8]) -> Option<u64> {
    (t.len() >= 56 && &t[..4] == b"HPET").then(|| u64_of(t, 44))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ecam {
    pub base: u64,
    pub segment: u16,
    pub start_bus: u8,
    pub end_bus: u8,
}

/// MCFG: where each PCIe segment's configuration space is.
pub fn mcfg(t: &[u8]) -> Vec<Ecam> {
    let mut out = Vec::new();
    if t.len() < 44 || &t[..4] != b"MCFG" {
        return out;
    }
    for o in (44..t.len()).step_by(16) {
        if o + 16 > t.len() {
            break;
        }
        out.push(Ecam { base: u64_of(t, o), segment: u16_of(t, o + 8), start_bus: t[o + 10], end_bus: t[o + 11] });
    }
    out
}

/// The firmware's own boot times (ns since reset), from FPDT → FBPT.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct FirmwareTimes {
    /// F0: the reset ended.
    pub reset_end: u64,
    /// F1: the OS loader's StartImage began.
    pub os_loader_start: u64,
}

pub fn fpdt(m: &mut impl Mem, t: &[u8]) -> Option<FirmwareTimes> {
    if t.len() < 36 || &t[..4] != b"FPDT" {
        return None;
    }
    let mut o = 36;
    while o + 16 <= t.len() {
        let (kind, len) = (u16_of(t, o), t[o + 2] as usize);
        if len < 4 || o + len > t.len() {
            break;
        }
        if kind == 0 && len >= 16 {
            // The basic boot performance table (FBPT): its boot record is type 2.
            let addr = u64_of(t, o + 8);
            let mut head = [0u8; 8];
            if !m.read(addr, &mut head) || &head[..4] != b"FBPT" {
                return None;
            }
            let flen = (u32_of(&head, 4) as usize).min(4096);
            let mut f = alloc::vec![0u8; flen];
            if !m.read(addr, &mut f) {
                return None;
            }
            let mut r = 8;
            while r + 4 <= f.len() {
                let (rk, rl) = (u16_of(&f, r), f[r + 2] as usize);
                if rl < 4 || r + rl > f.len() {
                    break;
                }
                if rk == 2 && rl >= 48 {
                    return Some(FirmwareTimes { reset_end: u64_of(&f, r + 8), os_loader_start: u64_of(&f, r + 24) });
                }
                r += rl;
            }
            return None;
        }
        o += len;
    }
    None
}

#[cfg(test)]
mod tests;
