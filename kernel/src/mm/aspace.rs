// SPDX-License-Identifier: Apache-2.0
//! Address spaces (TASK-025): a process's page tables. The lower half is the
//! process's own; the upper half is the kernel's, shared by every address space.
//!
//! - `map` puts a VMO's pages at an address, W^X: a mapping is never both
//!   writable and executable.
//! - SMEP and SMAP are on (when the CPU has them): the kernel can't run user
//!   code, and touches user memory only inside the user-copy windows (`user.rs`).
//! - Each address space has a PCID, so switching doesn't throw away the kernel's
//!   global TLB entries.

use super::frames::{self, FRAME};
use super::paging::{self, NX, USER, WRITABLE};
use super::vmo::Vmo;
use crate::arch::cpu;
use crate::sync::SpinLock;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU16, Ordering};
use fcore_abi::FErr;
use fcore_abi::layout::DIRECT_MAP;

/// The lowest address a mapping may start at (null and the first 4 MiB stay unmapped).
pub const USER_BASE: u64 = 0x40_0000;
/// One past the highest user address.
pub const USER_TOP: u64 = 0x0000_8000_0000_0000;

pub mod perm {
    pub const READ: u32 = 1;
    pub const WRITE: u32 = 2;
    pub const EXEC: u32 = 4;
}

struct Mapping {
    addr: u64,
    len: u64,
    _vmo: Arc<Vmo>,
}

pub struct AddressSpace {
    pub pml4: u64,
    pub pcid: u16,
    maps: SpinLock<Vec<Mapping>>,
}

static NEXT_PCID: AtomicU16 = AtomicU16::new(1);

/// Turns on SMEP, SMAP and PCIDs where the CPU has them.
pub fn init_cpu_protections() -> (bool, bool, bool) {
    let f7 = cpu::cpuid(7, 0).ebx;
    let smep = f7 & (1 << 7) != 0;
    let smap = f7 & (1 << 20) != 0;
    let pcid = cpu::cpuid(1, 0).ecx & (1 << 17) != 0;
    let mut cr4 = cpu::cr4();
    if smep {
        cr4 |= 1 << 20;
    }
    if smap {
        cr4 |= 1 << 21;
    }
    if pcid && cpu::cr3() & 0xFFF == 0 {
        cr4 |= 1 << 17;
    }
    // SAFETY: each bit is set only when CPUID reports the feature.
    unsafe { cpu::set_cr4(cr4) };
    super::user::set_smap(smap);
    (smep, smap, pcid)
}

impl AddressSpace {
    pub fn new() -> Result<Arc<AddressSpace>, FErr> {
        let pml4 = frames::alloc_zeroed().ok_or(FErr::NoMemory)?;
        // SAFETY: copy the kernel half (entries 256–511) from the kernel's PML4.
        unsafe {
            let src = (paging::kernel_pml4() + DIRECT_MAP) as *const u64;
            let dst = (pml4 + DIRECT_MAP) as *mut u64;
            core::ptr::copy_nonoverlapping(src.add(256), dst.add(256), 256);
        }
        let pcid = (NEXT_PCID.fetch_add(1, Ordering::Relaxed) % 4095) + 1;
        Ok(Arc::new(AddressSpace { pml4, pcid, maps: SpinLock::new(Vec::new()) }))
    }

    fn flags(perms: u32) -> Result<u64, FErr> {
        if perms & perm::WRITE != 0 && perms & perm::EXEC != 0 {
            return Err(FErr::InvalidArgs); // W^X
        }
        let mut f = USER;
        if perms & perm::WRITE != 0 {
            f |= WRITABLE;
        }
        if perms & perm::EXEC == 0 {
            f |= NX;
        }
        Ok(f)
    }

    fn free_range(maps: &[Mapping], addr: u64, len: u64) -> bool {
        addr >= USER_BASE
            && addr.checked_add(len).is_some_and(|e| e <= USER_TOP)
            && maps.iter().all(|m| addr + len <= m.addr || addr >= m.addr + m.len)
    }

    /// Maps `len` bytes of `vmo` from `vmo_off` at `hint` (or the first free
    /// place); returns the address.
    pub fn map(&self, vmo: &Arc<Vmo>, vmo_off: u64, len: u64, hint: u64, perms: u32) -> Result<u64, FErr> {
        let flags = Self::flags(perms)?;
        if len == 0
            || !vmo_off.is_multiple_of(FRAME)
            || vmo_off.checked_add(len).is_none_or(|e| e > vmo.size.next_multiple_of(FRAME))
        {
            return Err(FErr::InvalidArgs);
        }
        let len = len.next_multiple_of(FRAME);
        let mut maps = self.maps.lock();
        let addr = if hint != 0 {
            if !hint.is_multiple_of(FRAME) || !Self::free_range(&maps, hint, len) {
                return Err(FErr::InvalidArgs);
            }
            hint
        } else {
            let mut a = 0x1000_0000u64;
            loop {
                if Self::free_range(&maps, a, len) {
                    break a;
                }
                a = maps.iter().filter(|m| m.addr + m.len > a).map(|m| m.addr + m.len).min().ok_or(FErr::NoMemory)?;
                if a >= USER_TOP {
                    return Err(FErr::NoMemory);
                }
            }
        };
        for p in 0..len / FRAME {
            let phys = vmo.page(vmo_off + p * FRAME).ok_or(FErr::InvalidArgs)?;
            paging::map(self.pml4, addr + p * FRAME, phys, flags).map_err(|_| FErr::NoMemory)?;
        }
        maps.push(Mapping { addr, len, _vmo: vmo.clone() });
        Ok(addr)
    }

    /// Unmaps a whole mapping that starts at `addr` and is `len` long.
    pub fn unmap(&self, addr: u64, len: u64) -> Result<(), FErr> {
        let mut maps = self.maps.lock();
        let i =
            maps.iter().position(|m| m.addr == addr && m.len == len.next_multiple_of(FRAME)).ok_or(FErr::NotFound)?;
        let m = maps.remove(i);
        for p in 0..m.len / FRAME {
            paging::unmap(self.pml4, m.addr + p * FRAME);
        }
        Ok(())
    }

    /// Changes the permissions of a whole mapping (W^X still holds).
    pub fn protect(&self, addr: u64, len: u64, perms: u32) -> Result<(), FErr> {
        let flags = Self::flags(perms)?;
        let maps = self.maps.lock();
        let m = maps.iter().find(|m| m.addr == addr && m.len == len.next_multiple_of(FRAME)).ok_or(FErr::NotFound)?;
        for p in 0..m.len / FRAME {
            paging::protect(self.pml4, m.addr + p * FRAME, flags).map_err(|_| FErr::InvalidArgs)?;
        }
        Ok(())
    }

    /// Makes this the CPU's address space.
    pub fn activate(&self) {
        let pcid_on = cpu::cr4() & (1 << 17) != 0;
        let v = if pcid_on { self.pml4 | self.pcid as u64 } else { self.pml4 };
        // SAFETY: the upper half is the kernel's, so the kernel stays mapped.
        unsafe { cpu::set_cr3(v) };
    }
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        // Free the user half's table pages (the mapped frames belong to the VMOs).
        fn free_level(table: u64, level: u32) {
            for i in 0..512 {
                // SAFETY: a table frame of this address space, through the direct map.
                let e = unsafe { *((table + DIRECT_MAP) as *const u64).add(i) };
                if e & paging::PRESENT != 0 && e & paging::HUGE == 0 && level > 1 {
                    free_level(e & 0x000F_FFFF_FFFF_F000, level - 1);
                }
            }
            frames::free(table);
        }
        for i in 0..256 {
            // SAFETY: as above.
            let e = unsafe { *((self.pml4 + DIRECT_MAP) as *const u64).add(i) };
            if e & paging::PRESENT != 0 {
                free_level(e & 0x000F_FFFF_FFFF_F000, 3);
            }
        }
        frames::free(self.pml4);
    }
}

/// Back to the kernel's own tables.
pub fn activate_kernel() {
    // SAFETY: the kernel's PML4 maps everything the kernel uses.
    unsafe { cpu::set_cr3(paging::kernel_pml4()) };
}

ktest!(aspace_writable_and_executable_together_is_refused, {
    let a = AddressSpace::new()?;
    let v = Vmo::new(FRAME, false)?;
    match a.map(&v, 0, FRAME, 0, perm::READ | perm::WRITE | perm::EXEC) {
        Err(FErr::InvalidArgs) => Ok(()),
        _ => Err("a W+X mapping was allowed"),
    }
});

ktest!(aspace_the_kernel_cannot_run_or_touch_user_pages_outside_a_window, {
    let (smep, smap, _) = init_cpu_protections();
    let a = AddressSpace::new()?;
    let code = Vmo::new(FRAME, false)?;
    code.write(0, &[0xC3])?; // ret
    let at = a.map(&code, 0, FRAME, 0x50_0000, perm::READ | perm::EXEC)?;
    let data = Vmo::new(FRAME, false)?;
    data.write(0, b"freally")?;
    let d = a.map(&data, 0, FRAME, 0x60_0000, perm::READ | perm::WRITE)?;
    a.activate();
    let exec = crate::exceptions::try_exec(at);
    let read = crate::exceptions::try_read(d);
    let mut buf = [0u8; 7];
    let copied = super::user::copy_from_user(&mut buf, d);
    activate_kernel();
    if smep && exec != Err(14) {
        return Err("the kernel ran a user page (SMEP is off?)");
    }
    if smap && read != Err(14) {
        return Err("the kernel read a user page outside a copy window (SMAP is off?)");
    }
    if copied.is_err() || &buf != b"freally" {
        return Err("copy_from_user couldn't read a mapped user page");
    }
    if !smep || !smap {
        klog!("    note: this CPU lacks SMEP ({smep}) or SMAP ({smap}); those checks were skipped");
    }
    Ok(())
});

ktest!(aspace_copies_refuse_kernel_and_unmapped_addresses, {
    let mut buf = [0u8; 8];
    if super::user::copy_from_user(&mut buf, fcore_abi::layout::DIRECT_MAP).is_ok() {
        return Err("copy_from_user read a kernel address");
    }
    let a = AddressSpace::new()?;
    a.activate();
    let r = super::user::copy_from_user(&mut buf, 0x7000_0000);
    activate_kernel();
    if r.is_ok() { Err("copy_from_user read an unmapped address") } else { Ok(()) }
});

ktest!(aspace_dropping_one_returns_its_table_pages, {
    let before = frames::free_count();
    {
        let a = AddressSpace::new()?;
        let v = Vmo::new(16 * FRAME, false)?;
        a.map(&v, 0, 16 * FRAME, 0, perm::READ | perm::WRITE)?;
        a.map(&v, 0, FRAME, 0x7FFF_0000_0000, perm::READ)?;
    }
    if frames::free_count() != before { Err("an address space leaked frames") } else { Ok(()) }
});
