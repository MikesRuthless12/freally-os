// SPDX-License-Identifier: Apache-2.0
//! Page tables (TASK-020): the kernel's own, and the operations every address
//! space uses (map, unmap, translate).
//!
//! The kernel's tables replace the bootloader's: the direct map of all physical
//! memory (1 GiB pages when the CPU has them, else 2 MiB; read/write, never
//! executable, global) and the kernel's sections at their linked addresses
//! with exactly their permissions (text R-X, rodata R, data and bss RW). The
//! identity map is gone after the switch.

use super::frames;
use crate::arch::cpu;
use fcore_abi::layout::DIRECT_MAP;
use fcore_abi::{BootInfo, MemRegion};

pub const PRESENT: u64 = 1;
pub const WRITABLE: u64 = 1 << 1;
pub const USER: u64 = 1 << 2;
pub const PWT: u64 = 1 << 3;
pub const PCD: u64 = 1 << 4;
pub const HUGE: u64 = 1 << 7;
pub const GLOBAL: u64 = 1 << 8;
pub const NX: u64 = 1 << 63;
const ADDR: u64 = 0x000F_FFFF_FFFF_F000;

unsafe extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __kernel_end: u8;
}

fn entries(table: u64) -> *mut u64 {
    (table + DIRECT_MAP) as *mut u64
}

/// The table an entry points at, made (zeroed) if missing.
fn next(table: u64, i: u64, user: bool) -> Result<u64, &'static str> {
    // SAFETY: `table` is a page-table frame, reached through the direct map.
    unsafe {
        let e = entries(table).add(i as usize);
        if *e & PRESENT == 0 {
            let t = frames::alloc_zeroed().ok_or("out of memory for page tables")?;
            *e = t | PRESENT | WRITABLE | if user { USER } else { 0 };
        } else if *e & HUGE != 0 {
            return Err("a huge page is in the way");
        }
        Ok(*e & ADDR)
    }
}

/// Maps one 4 KiB page. `flags` must not ask for writable and executable together
/// (W^X: a writable page must carry NX).
pub fn map(pml4: u64, virt: u64, phys: u64, flags: u64) -> Result<(), &'static str> {
    if flags & WRITABLE != 0 && flags & NX == 0 {
        return Err("W^X: a page can't be writable and executable");
    }
    let user = flags & USER != 0;
    let pdpt = next(pml4, (virt >> 39) & 511, user)?;
    let pd = next(pdpt, (virt >> 30) & 511, user)?;
    let pt = next(pd, (virt >> 21) & 511, user)?;
    // SAFETY: `pt` is a page table frame.
    unsafe { *entries(pt).add(((virt >> 12) & 511) as usize) = (phys & ADDR) | flags | PRESENT };
    invalidate(virt);
    Ok(())
}

/// Unmaps one 4 KiB page; returns the frame it mapped.
pub fn unmap(pml4: u64, virt: u64) -> Option<u64> {
    let e = leaf(pml4, virt)?;
    // SAFETY: `e` is a present 4 KiB leaf entry.
    unsafe {
        let phys = *e & ADDR;
        *e = 0;
        invalidate(virt);
        Some(phys)
    }
}

/// Changes a mapped page's flags (W^X still holds).
pub fn protect(pml4: u64, virt: u64, flags: u64) -> Result<(), &'static str> {
    if flags & WRITABLE != 0 && flags & NX == 0 {
        return Err("W^X: a page can't be writable and executable");
    }
    let e = leaf(pml4, virt).ok_or("not mapped")?;
    // SAFETY: a present 4 KiB leaf.
    unsafe { *e = (*e & ADDR) | flags | PRESENT };
    invalidate(virt);
    Ok(())
}

/// The 4 KiB leaf entry for `virt`, if present.
fn leaf(pml4: u64, virt: u64) -> Option<*mut u64> {
    let mut t = pml4;
    for shift in [39u64, 30, 21] {
        // SAFETY: walking present tables through the direct map.
        let e = unsafe { *entries(t).add(((virt >> shift) & 511) as usize) };
        if e & PRESENT == 0 || e & HUGE != 0 {
            return None;
        }
        t = e & ADDR;
    }
    // SAFETY: as above.
    let e = unsafe { entries(t).add(((virt >> 12) & 511) as usize) };
    // SAFETY: as above.
    if unsafe { *e } & PRESENT == 0 { None } else { Some(e) }
}

/// The physical address and flags behind `virt` (huge pages included).
pub fn translate(pml4: u64, virt: u64) -> Option<(u64, u64)> {
    let mut t = pml4;
    for (shift, size) in [(39u64, 0u64), (30, 1 << 30), (21, 1 << 21), (12, 1 << 12)] {
        // SAFETY: walking present tables through the direct map.
        let e = unsafe { *entries(t).add(((virt >> shift) & 511) as usize) };
        if e & PRESENT == 0 {
            return None;
        }
        if shift == 12 || (e & HUGE != 0 && size != 0) {
            let base = e & ADDR & !(size - 1);
            return Some((base + (virt & (size - 1)), e & !ADDR));
        }
        t = e & ADDR;
    }
    None
}

fn invalidate(virt: u64) {
    // SAFETY: invlpg only drops a TLB entry.
    unsafe { core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags)) };
}

static mut KERNEL_PML4: u64 = 0;

/// The kernel's PML4 (every address space shares its upper half).
pub fn kernel_pml4() -> u64 {
    // SAFETY: written once at init, read-only afterwards.
    unsafe { KERNEL_PML4 }
}

fn sym(s: &u8) -> u64 {
    s as *const u8 as u64
}

/// Builds the kernel's tables and switches to them.
pub fn init(info: &BootInfo, regions: &[MemRegion]) -> Result<(), &'static str> {
    let pml4 = frames::alloc_zeroed().ok_or("out of memory")?;
    // The direct map: everything up to the highest region or the framebuffer.
    let top =
        regions.iter().map(|r| r.base + r.len).max().unwrap_or(0).max(info.framebuffer.base + info.framebuffer.size);
    let gib = top.div_ceil(1 << 30).max(4);
    let big = cpu::cpuid(0x8000_0001, 0).edx & (1 << 26) != 0;
    for g in 0..gib {
        let v = DIRECT_MAP + (g << 30);
        let pdpt = next(pml4, (v >> 39) & 511, false)?;
        // SAFETY: fresh table frames, reached through the direct map.
        unsafe {
            let e = entries(pdpt).add(((v >> 30) & 511) as usize);
            if big {
                *e = (g << 30) | PRESENT | WRITABLE | HUGE | GLOBAL | NX;
            } else {
                let pd = frames::alloc_zeroed().ok_or("out of memory")?;
                for m in 0..512u64 {
                    *entries(pd).add(m as usize) = (g << 30) | (m << 21) | PRESENT | WRITABLE | HUGE | GLOBAL | NX;
                }
                *e = pd | PRESENT | WRITABLE;
            }
        }
    }
    // The kernel's sections, at the physical place the bootloader loaded them.
    // SAFETY: linker-script symbols.
    let (ts, te, rs, re, ds, ke) = unsafe {
        (
            sym(&__text_start),
            sym(&__text_end),
            sym(&__rodata_start),
            sym(&__rodata_end),
            sym(&__data_start),
            sym(&__kernel_end),
        )
    };
    let phys_of = |v: u64| info.kernel.phys + (v - info.kernel.virt);
    for (from, to, flags) in [(ts, te, 0), (rs, re, NX), (ds, ke, WRITABLE | NX)] {
        let mut v = from & !0xFFF;
        while v < to {
            map(pml4, v, phys_of(v), flags | GLOBAL)?;
            v += 4096;
        }
    }
    // SAFETY: the new tables map the kernel, its stack (in the direct map) and BootInfo.
    unsafe {
        cpu::set_cr4(cpu::cr4() | (1 << 7)); // PGE: global pages survive address-space switches
        cpu::set_cr0(cpu::cr0() | (1 << 16)); // WP: the kernel honours read-only pages too
        cpu::set_cr3(pml4);
        KERNEL_PML4 = pml4;
    }
    Ok(())
}

ktest!(paging_kernel_text_is_read_only_and_data_is_not_executable, {
    // SAFETY: linker symbols.
    let (text, data) = unsafe { (sym(&__text_start), sym(&__data_start)) };
    let (_, tf) = translate(kernel_pml4(), text).ok_or("text unmapped")?;
    let (_, df) = translate(kernel_pml4(), data).ok_or("data unmapped")?;
    if tf & WRITABLE != 0 || tf & NX != 0 {
        return Err("kernel text is writable or not executable");
    }
    if df & NX == 0 || df & WRITABLE == 0 {
        return Err("kernel data is executable or read-only");
    }
    // SAFETY: a write to text must fault; the probe turns the fault into Err.
    if unsafe { crate::exceptions::try_write(text) } != Err(14) {
        return Err("writing kernel text didn't fault");
    }
    Ok(())
});

ktest!(paging_the_identity_map_is_gone, {
    // Low memory is reachable only through the direct map now.
    if crate::exceptions::try_read(0x10_0000).is_ok() {
        return Err("the identity map still exists");
    }
    crate::exceptions::try_read(DIRECT_MAP + 0x10_0000).map_err(|_| "the direct map doesn't cover low memory")
});

ktest!(paging_w_and_x_are_refused_together, {
    let p = frames::alloc_zeroed().ok_or("no frame")?;
    let r = map(kernel_pml4(), 0xFFFF_C000_0000_0000, p, WRITABLE);
    frames::free(p);
    if r.is_ok() { Err("a writable, executable page was mapped") } else { Ok(()) }
});
