// SPDX-License-Identifier: Apache-2.0
//! What devmgr finds: ACPI tables, PCIe functions, the framebuffer, and the
//! resources each driver gets.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::{BootInfo, FErr, cache, perm, resource, well_known};
use fstd::handle::Handle;
use fwire::HandleRef;
use fwire::proto::devmgr as d;

/// Physical memory through read-only, write-back mappings of the root resource.
struct Phys {
    root: u32,
    pages: BTreeMap<u64, u64>,
}

impl Phys {
    fn page(&mut self, page: u64) -> Option<u64> {
        if let Some(&va) = self.pages.get(&page) {
            return Some(va);
        }
        let va = fstd::sys::call1(
            fcore_abi::syscall::MMIO_MAP,
            [self.root as u64, page, 4096, cache::WRITE_BACK | cache::READ_ONLY, 0, 0],
        )
        .ok()?;
        self.pages.insert(page, va);
        Some(va)
    }
}

impl facpi::Mem for Phys {
    fn read(&mut self, addr: u64, buf: &mut [u8]) -> bool {
        for (i, b) in buf.iter_mut().enumerate() {
            let a = addr + i as u64;
            let Some(va) = self.page(a & !0xFFF) else { return false };
            // SAFETY: a read-only mapping of that page, kept for devmgr's life.
            *b = unsafe { core::ptr::read_volatile((va + (a & 0xFFF)) as *const u8) };
        }
        true
    }
}

pub struct Hardware {
    root: Handle,
    pub tables: Vec<facpi::TableRef>,
    pub pci: Vec<d::PciDevice>,
    pub cpus: u32,
    pub has_8042: bool,
    pub firmware: d::FirmwareTimes,
    framebuffer: (u64, u64, u32, u32, u32, u32),
    madt: Vec<u8>,
}

/// Reads a u8/u16 from a mapped configuration space.
fn cfg8(va: u64, off: u64) -> u8 {
    // SAFETY: inside a mapped 4 KiB configuration page.
    unsafe { core::ptr::read_volatile((va + off) as *const u8) }
}
fn cfg16(va: u64, off: u64) -> u16 {
    // SAFETY: as above, 2-aligned.
    unsafe { core::ptr::read_volatile((va + off) as *const u16) }
}

impl Hardware {
    pub fn find(root: Handle, bootinfo: Handle) -> Result<Hardware, String> {
        let at = fstd::sys::vm_map(well_known::SELF_ASPACE, bootinfo.raw(), 0, 4096, 0, perm::READ)
            .map_err(|e| alloc::format!("BootInfo: {}", e.describe()))?;
        // SAFETY: conductor's copy of BootInfo, at offset 0 of that page.
        let bi: BootInfo = unsafe { core::ptr::read_unaligned(at as *const BootInfo) };
        let mut mem = Phys { root: root.raw(), pages: BTreeMap::new() };
        let tables =
            facpi::tables(&mut mem, bi.acpi_rsdp).ok_or("the firmware's ACPI tables are missing or damaged")?;
        let get = |mem: &mut Phys, sig: &[u8; 4]| {
            tables.iter().find(|t| &t.signature == sig).and_then(|t| facpi::table(mem, t.addr))
        };
        let madt = get(&mut mem, b"APIC").unwrap_or_default();
        let cpus = facpi::madt(&madt).map_or(1, |m| m.cpus);
        let has_8042 = get(&mut mem, b"FACP").and_then(|t| facpi::fadt(&t)).is_none_or(|f| f.has_8042);
        let firmware = get(&mut mem, b"FPDT").and_then(|t| facpi::fpdt(&mut mem, &t)).map_or(
            d::FirmwareTimes { reset_end: 0, os_loader_start: 0 },
            |f| d::FirmwareTimes { reset_end: f.reset_end, os_loader_start: f.os_loader_start },
        );
        let mut pci = Vec::new();
        for e in get(&mut mem, b"MCFG").map(|t| facpi::mcfg(&t)).unwrap_or_default() {
            scan(root.raw(), &e, e.start_bus, &mut pci, 0);
        }
        let fb = &bi.framebuffer;
        Ok(Hardware {
            root,
            tables,
            pci,
            cpus,
            has_8042,
            firmware,
            framebuffer: (fb.base, fb.size, fb.width, fb.height, fb.stride_px, fb.format as u32),
            madt,
        })
    }

    pub fn table_list(&self) -> d::Tables {
        d::Tables {
            tables: self
                .tables
                .iter()
                .map(|t| (String::from_utf8_lossy(&t.signature).into_owned(), (t.addr, t.len)))
                .collect(),
        }
    }

    fn carve(&self, kind: u64, base: u64, len: u64) -> Result<Handle, FErr> {
        Handle::from_raw(fstd::sys::resource_create(self.root.raw(), kind, base, len)?).ok_or(FErr::Internal)
    }

    /// Exactly what one driver needs, by the name conductor gave its connection.
    pub fn resources_for(&self, driver: &str) -> Result<(d::Resources, Vec<Handle>), FErr> {
        let mut handles = Vec::new();
        let mut named = Vec::new();
        let mut values: Vec<(String, u64)> = Vec::new();
        let mut add = |name: &str, h: Handle, handles: &mut Vec<Handle>| {
            named.push((String::from(name), HandleRef(handles.len() as u8)));
            handles.push(h);
        };
        match driver {
            "drv-gop-fb" => {
                let (base, size, w, h, stride, format) = self.framebuffer;
                add("framebuffer", self.carve(resource::MMIO, base, size.next_multiple_of(4096))?, &mut handles);
                for (k, v) in [
                    ("base", base),
                    ("size", size),
                    ("width", w as u64),
                    ("height", h as u64),
                    ("stride", stride as u64),
                    ("format", format as u64),
                ] {
                    values.push((String::from(k), v));
                }
            }
            "drv-i8042" if self.has_8042 => {
                add("data", self.carve(resource::IO_PORT, 0x60, 1)?, &mut handles);
                add("command", self.carve(resource::IO_PORT, 0x64, 1)?, &mut handles);
                for (name, irq) in [("keyboard-irq", 1u8), ("mouse-irq", 12)] {
                    let (gsi, flags) = facpi::isa_gsi(&self.madt, irq);
                    add(name, self.carve(resource::IRQ, gsi as u64, 1)?, &mut handles);
                    values.push((alloc::format!("{name}-gsi"), gsi as u64));
                    values.push((alloc::format!("{name}-flags"), flags as u64));
                }
            }
            "drv-uart16550" => {
                for (name, port, irq) in [("com1", 0x3F8u64, 4u8), ("com2", 0x2F8, 3), ("com3", 0x3E8, 4)] {
                    add(name, self.carve(resource::IO_PORT, port, 8)?, &mut handles);
                    let (gsi, flags) = facpi::isa_gsi(&self.madt, irq);
                    values.push((alloc::format!("{name}-gsi"), gsi as u64));
                    values.push((alloc::format!("{name}-flags"), flags as u64));
                }
                let (gsi4, _) = facpi::isa_gsi(&self.madt, 4);
                let (gsi3, _) = facpi::isa_gsi(&self.madt, 3);
                add("irq4", self.carve(resource::IRQ, gsi4 as u64, 1)?, &mut handles);
                add("irq3", self.carve(resource::IRQ, gsi3 as u64, 1)?, &mut handles);
            }
            _ => return Err(FErr::NotFound),
        }
        Ok((d::Resources { handles: named, values }, handles))
    }
}

/// Every function on `bus` and the buses behind its bridges.
fn scan(root: u32, e: &facpi::Ecam, bus: u8, out: &mut Vec<d::PciDevice>, depth: u32) {
    if depth > 32 || bus < e.start_bus || bus > e.end_bus {
        return;
    }
    let bus_base = e.base + ((bus - e.start_bus) as u64) * (1 << 20);
    for dev in 0..32u64 {
        for func in 0..8u64 {
            let phys = bus_base + (dev << 15) + (func << 12);
            let Ok(va) = fstd::sys::call1(
                fcore_abi::syscall::MMIO_MAP,
                [root as u64, phys, 4096, cache::UNCACHED | cache::READ_ONLY, 0, 0],
            ) else {
                return;
            };
            let vendor = cfg16(va, 0);
            let header = cfg8(va, 0x0E);
            let present = vendor != 0xFFFF;
            if present {
                out.push(d::PciDevice {
                    segment: e.segment,
                    bus,
                    device: dev as u8,
                    function: func as u8,
                    vendor,
                    id: cfg16(va, 2),
                    class: cfg8(va, 0x0B),
                    subclass: cfg8(va, 0x0A),
                    prog_if: cfg8(va, 0x09),
                });
                if header & 0x7F == 1 {
                    let secondary = cfg8(va, 0x19);
                    if secondary > bus {
                        scan(root, e, secondary, out, depth + 1);
                    }
                }
            }
            let _ = fstd::sys::call1(fcore_abi::syscall::VM_UNMAP, [well_known::SELF_ASPACE as u64, va, 4096, 0, 0, 0]);
            // Function 0 absent, or not multi-function: the other seven aren't there.
            if func == 0 && (!present || header & 0x80 == 0) {
                break;
            }
        }
    }
}
