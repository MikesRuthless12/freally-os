// SPDX-License-Identifier: Apache-2.0
//! The ACPI tables the kernel itself needs (TASK-023): the MADT (CPUs, I/O
//! APICs, interrupt overrides) and the HPET. Everything else in ACPI is
//! devmgr's (TASK-041). Tables are read through the direct map, and every
//! table's checksum is checked before it's trusted.

use fcore_abi::layout::DIRECT_MAP;

pub const MAX_CPUS: usize = 256;
pub const MAX_IOAPICS: usize = 8;
pub const MAX_OVERRIDES: usize = 24;

#[derive(Clone, Copy, Debug, Default)]
pub struct IoApic {
    pub id: u8,
    pub addr: u64,
    pub gsi_base: u32,
}

/// An ISA IRQ wired to another GSI, and its polarity and trigger.
#[derive(Clone, Copy, Debug, Default)]
pub struct Override {
    pub irq: u8,
    pub gsi: u32,
    pub flags: u16,
}

pub struct Madt {
    pub lapic_addr: u64,
    pub cpus: [u32; MAX_CPUS],
    pub cpu_count: usize,
    pub ioapics: [IoApic; MAX_IOAPICS],
    pub ioapic_count: usize,
    pub overrides: [Override; MAX_OVERRIDES],
    pub override_count: usize,
    /// The legacy PICs exist (PCAT_COMPAT) and must be masked.
    pub has_8259: bool,
}

pub struct Tables {
    pub madt: Madt,
    pub hpet: Option<u64>,
}

fn phys<T>(addr: u64) -> *const T {
    (addr + DIRECT_MAP) as *const T
}

fn read<T: Copy>(addr: u64) -> T {
    // SAFETY: ACPI tables lie in firmware memory the direct map covers; reads are unaligned-safe.
    unsafe { core::ptr::read_unaligned(phys::<T>(addr)) }
}

fn checksum_ok(addr: u64, len: u32) -> bool {
    let mut sum = 0u8;
    for i in 0..len as u64 {
        sum = sum.wrapping_add(read::<u8>(addr + i));
    }
    sum == 0
}

/// Every table the XSDT (or RSDT) lists, as (signature, address, length).
fn tables(rsdp: u64) -> Result<impl Iterator<Item = ([u8; 4], u64, u32)>, &'static str> {
    if rsdp == 0 || read::<[u8; 8]>(rsdp) != *b"RSD PTR " || !checksum_ok(rsdp, 20) {
        return Err("no valid RSDP");
    }
    let revision = read::<u8>(rsdp + 15);
    let (root, entry) = if revision >= 2 && checksum_ok(rsdp, read::<u32>(rsdp + 20)) {
        (read::<u64>(rsdp + 24), 8u64)
    } else {
        (read::<u32>(rsdp + 16) as u64, 4u64)
    };
    let len = read::<u32>(root + 4);
    if !checksum_ok(root, len) {
        return Err("the root table's checksum is wrong");
    }
    let n = (len as u64 - 36) / entry;
    Ok((0..n).filter_map(move |i| {
        let p = root + 36 + i * entry;
        let t = if entry == 8 { read::<u64>(p) } else { read::<u32>(p) as u64 };
        let tlen = read::<u32>(t + 4);
        checksum_ok(t, tlen).then(|| (read::<[u8; 4]>(t), t, tlen))
    }))
}

pub fn parse(rsdp: u64) -> Result<Tables, &'static str> {
    let mut madt = Madt {
        lapic_addr: 0,
        cpus: [0; MAX_CPUS],
        cpu_count: 0,
        ioapics: [IoApic::default(); MAX_IOAPICS],
        ioapic_count: 0,
        overrides: [Override::default(); MAX_OVERRIDES],
        override_count: 0,
        has_8259: false,
    };
    let mut hpet = None;
    let mut found_madt = false;
    for (sig, t, len) in tables(rsdp)? {
        match &sig {
            b"APIC" => {
                found_madt = true;
                madt.lapic_addr = read::<u32>(t + 36) as u64;
                madt.has_8259 = read::<u32>(t + 40) & 1 != 0;
                let mut p = t + 44;
                while p + 2 <= t + len as u64 {
                    let (kind, elen) = (read::<u8>(p), read::<u8>(p + 1) as u64);
                    if elen < 2 {
                        break;
                    }
                    match kind {
                        // Processor local APIC: usable if enabled or online-capable.
                        0 if read::<u32>(p + 4) & 3 != 0 && madt.cpu_count < MAX_CPUS => {
                            madt.cpus[madt.cpu_count] = read::<u8>(p + 3) as u32;
                            madt.cpu_count += 1;
                        }
                        1 if madt.ioapic_count < MAX_IOAPICS => {
                            madt.ioapics[madt.ioapic_count] = IoApic {
                                id: read::<u8>(p + 2),
                                addr: read::<u32>(p + 4) as u64,
                                gsi_base: read::<u32>(p + 8),
                            };
                            madt.ioapic_count += 1;
                        }
                        2 if madt.override_count < MAX_OVERRIDES => {
                            madt.overrides[madt.override_count] =
                                Override { irq: read::<u8>(p + 3), gsi: read::<u32>(p + 4), flags: read::<u16>(p + 8) };
                            madt.override_count += 1;
                        }
                        5 => madt.lapic_addr = read::<u64>(p + 4),
                        // Processor local x2APIC.
                        9 if read::<u32>(p + 8) & 3 != 0 && madt.cpu_count < MAX_CPUS => {
                            let id = read::<u32>(p + 4);
                            if !madt.cpus[..madt.cpu_count].contains(&id) {
                                madt.cpus[madt.cpu_count] = id;
                                madt.cpu_count += 1;
                            }
                        }
                        _ => {}
                    }
                    p += elen;
                }
            }
            b"HPET" => hpet = Some(read::<u64>(t + 44)),
            _ => {}
        }
    }
    if !found_madt {
        return Err("no MADT");
    }
    Ok(Tables { madt, hpet })
}

/// The GSI an ISA IRQ arrives on, and its override flags (0 when none).
pub fn isa_gsi(madt: &Madt, irq: u8) -> (u32, u16) {
    madt.overrides[..madt.override_count].iter().find(|o| o.irq == irq).map_or((irq as u32, 0), |o| (o.gsi, o.flags))
}

ktest!(acpi_the_madt_lists_this_cpu, {
    let t = crate::acpi().ok_or("no ACPI tables")?;
    let me = crate::arch::apic::id();
    if t.madt.cpu_count == 0 || !t.madt.cpus[..t.madt.cpu_count].contains(&me) {
        return Err("the MADT doesn't list the boot CPU");
    }
    if t.madt.ioapic_count == 0 {
        return Err("no I/O APIC");
    }
    Ok(())
});
