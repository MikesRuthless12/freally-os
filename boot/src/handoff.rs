// SPDX-License-Identifier: Apache-2.0
//! The handoff to Freally Core (TASK-018).
//!
//! Before `ExitBootServices`, everything is allocated: the kernel's segments
//! (loaded from the verified image's `/system/kernel` ELF, W^X), the page
//! tables, `BootInfo`, the memory-map copy and the boot stack. Then the final
//! memory map is read and boot services exit (retrying once if the map key
//! changed). After that nothing calls boot services: the map is converted and
//! coalesced, the runtime services move to the direct map
//! (`SetVirtualAddressMap`), and the CPU switches to the new tables and jumps to
//! the kernel with `BootInfo` in RDI.
//!
//! The page tables:
//! - **identity** for all of physical memory (the bootloader runs there until the jump);
//! - **the direct map** at `DIRECT_MAP` (the same tables, entered through a
//!   no-execute PML4 entry);
//! - **the kernel** at `KERNEL_BASE`, 4 KiB pages, each segment's own permissions.

use crate::uefi::*;
use core::ptr::null_mut;
use fcore_abi::layout::{BOOT_STACK, DIRECT_MAP, KERNEL_BASE};
use fcore_abi::{
    BootInfo, BootStamps, Flavour, Framebuffer, ImageLoc, KernelLoc, MemKind, MemRegion, PixelFormat, Slice, memtype,
};

const PAGE: u64 = 4096;
const P: u64 = 1; // present
const W: u64 = 1 << 1; // writable
const PS: u64 = 1 << 7; // a 2 MiB or 1 GiB page
const NX: u64 = 1 << 63;
const ADDR: u64 = 0x000F_FFFF_FFFF_F000;

/// Why the handoff couldn't happen (shown on the integrity screen).
pub type Why = &'static str;

/// One loaded segment's place and permissions.
#[derive(Clone, Copy)]
struct Segment {
    vaddr: u64,
    pages: u64,
    phys: u64,
    writable: bool,
    executable: bool,
}

/// Freally Core, loaded into its own pages.
pub struct Kernel {
    loc: KernelLoc,
    entry: u64,
    segments: [Segment; 8],
    count: usize,
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(o)?, *b.get(o + 1)?]))
}
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

fn alloc(bs: &BootServices, mem: u32, pages: u64) -> Result<u64, Why> {
    let mut addr = 0u64;
    // The firmware's own type, the use noted in the ledger (see `ledger`).
    if is_error((bs.allocate_pages)(ALLOCATE_ANY_PAGES, LOADER_DATA, pages as usize, &mut addr)) {
        return Err("There isn’t enough memory to start Freally Core.");
    }
    if !crate::ledger::note(addr, pages, mem) {
        return Err("Freally OS made more allocations than it keeps track of.");
    }
    // SAFETY: `pages` pages at `addr` were just allocated to us.
    unsafe { core::ptr::write_bytes(addr as *mut u8, 0, (pages * PAGE) as usize) };
    Ok(addr)
}

/// Loads the kernel's ELF (already verified as part of the image).
pub fn load_kernel(bs: &BootServices, elf: &[u8]) -> Result<Kernel, Why> {
    let bad = "Freally Core’s program file is damaged.";
    if elf.get(0..4) != Some(b"\x7FELF") || elf.get(4) != Some(&2) || elf.get(5) != Some(&1) {
        return Err(bad);
    }
    if u16_at(elf, 16) != Some(2) || u16_at(elf, 18) != Some(0x3E) {
        return Err(bad); // ET_EXEC, x86-64
    }
    let entry = u64_at(elf, 24).ok_or(bad)?;
    let phoff = u64_at(elf, 32).ok_or(bad)? as usize;
    let phentsize = u16_at(elf, 54).ok_or(bad)? as usize;
    let phnum = u16_at(elf, 56).ok_or(bad)? as usize;
    if phentsize < 56 || phnum > 32 {
        return Err(bad);
    }
    let (mut lo, mut hi) = (u64::MAX, 0u64);
    let mut segs = [Segment { vaddr: 0, pages: 0, phys: 0, writable: false, executable: false }; 8];
    let mut count = 0;
    let mut raw: [(u64, u64, u64, u64, u32); 8] = [(0, 0, 0, 0, 0); 8];
    for i in 0..phnum {
        let h = phoff + i * phentsize;
        if u32_at(elf, h).ok_or(bad)? != 1 {
            continue; // PT_LOAD only
        }
        let flags = u32_at(elf, h + 4).ok_or(bad)?;
        let off = u64_at(elf, h + 8).ok_or(bad)?;
        let vaddr = u64_at(elf, h + 16).ok_or(bad)?;
        let filesz = u64_at(elf, h + 32).ok_or(bad)?;
        let memsz = u64_at(elf, h + 40).ok_or(bad)?;
        if flags & 1 != 0 && flags & 2 != 0 {
            return Err("Freally Core has a segment that’s both writable and executable."); // W^X
        }
        if vaddr < KERNEL_BASE
            || filesz > memsz
            || off.checked_add(filesz).is_none_or(|e| e > elf.len() as u64)
            || count == 8
        {
            return Err(bad);
        }
        lo = lo.min(vaddr & !(PAGE - 1));
        hi = hi.max(vaddr.checked_add(memsz).ok_or(bad)?.div_ceil(PAGE) * PAGE);
        raw[count] = (off, vaddr, filesz, memsz, flags);
        count += 1;
    }
    if count == 0 || hi <= lo || hi - lo > 64 << 20 {
        return Err(bad);
    }
    let len = hi - lo;
    let phys = alloc(bs, memtype::KERNEL, len / PAGE)?;
    let mut entry_ok = false;
    for (i, &(off, vaddr, filesz, memsz, flags)) in raw[..count].iter().enumerate() {
        // SAFETY: the destination lies inside the `len` bytes just allocated.
        unsafe {
            core::ptr::copy_nonoverlapping(
                elf.as_ptr().add(off as usize),
                (phys + (vaddr - lo)) as *mut u8,
                filesz as usize,
            );
        }
        let start = vaddr & !(PAGE - 1);
        let end = (vaddr + memsz).div_ceil(PAGE) * PAGE;
        segs[i] = Segment {
            vaddr: start,
            pages: (end - start) / PAGE,
            phys: phys + (start - lo),
            writable: flags & 2 != 0,
            executable: flags & 1 != 0,
        };
        if flags & 1 != 0 && (vaddr..vaddr + memsz).contains(&entry) {
            entry_ok = true;
        }
    }
    if !entry_ok {
        return Err(bad);
    }
    Ok(Kernel { loc: KernelLoc { phys, virt: lo, len }, entry, segments: segs, count })
}

/// A bump allocator over pre-allocated, zeroed pages, for page tables.
struct Pool {
    next: u64,
    end: u64,
}

impl Pool {
    fn page(&mut self) -> Result<u64, Why> {
        if self.next >= self.end {
            return Err("Freally OS ran out of room for its page tables.");
        }
        let p = self.next;
        self.next += PAGE;
        Ok(p)
    }
}

/// The entry at `index` of the table at `table`, made to point at a new table if empty.
unsafe fn next_table(pool: &mut Pool, table: u64, index: usize, flags: u64) -> Result<u64, Why> {
    let e = (table as *mut u64).wrapping_add(index);
    // SAFETY: `table` is a page-table page from the pool, identity-mapped.
    unsafe {
        if *e & P == 0 {
            *e = pool.page()? | flags;
        }
        Ok(*e & ADDR)
    }
}

fn has_1g_pages() -> bool {
    let r = core::arch::x86_64::__cpuid(0x8000_0001);
    r.edx & (1 << 26) != 0
}

/// Builds the tables; returns the PML4's physical address.
fn build_tables(pool: &mut Pool, max_phys: u64, k: &Kernel) -> Result<u64, Why> {
    let pml4 = pool.page()?;
    let gib = max_phys.div_ceil(1 << 30).max(4);
    let big = has_1g_pages();
    // The physical-memory map, shared by the identity and the direct map.
    for g in 0..gib {
        let l4 = (g / 512) as usize;
        // SAFETY: tables come from the pool and are identity-mapped.
        unsafe {
            let pdpt = next_table(pool, pml4, l4, P | W)?;
            let pml4e = (pml4 as *mut u64).add(256 + l4);
            *pml4e = pdpt | P | W | NX;
            let slot = (pdpt as *mut u64).add((g % 512) as usize);
            if big {
                *slot = (g << 30) | P | W | PS;
            } else {
                let pd = next_table(pool, pdpt, (g % 512) as usize, P | W)?;
                for m in 0..512u64 {
                    *(pd as *mut u64).add(m as usize) = (g << 30) | (m << 21) | P | W | PS;
                }
            }
        }
    }
    // The kernel, page by page.
    for s in &k.segments[..k.count] {
        for p in 0..s.pages {
            let v = s.vaddr + p * PAGE;
            let (i4, i3, i2, i1) = ((v >> 39) & 511, (v >> 30) & 511, (v >> 21) & 511, (v >> 12) & 511);
            // SAFETY: as above.
            unsafe {
                let pdpt = next_table(pool, pml4, i4 as usize, P | W)?;
                let pd = next_table(pool, pdpt, i3 as usize, P | W)?;
                let pt = next_table(pool, pd, i2 as usize, P | W)?;
                let mut flags = P;
                if s.writable {
                    flags |= W;
                }
                if !s.executable {
                    flags |= NX;
                }
                *(pt as *mut u64).add(i1 as usize) = (s.phys + p * PAGE) | flags;
            }
        }
    }
    Ok(pml4)
}

/// How many page-table pages `build_tables` can need.
fn pool_pages(max_phys: u64, k: &Kernel) -> u64 {
    let gib = max_phys.div_ceil(1 << 30).max(4);
    let maps = if has_1g_pages() { gib.div_ceil(512) } else { gib.div_ceil(512) + gib };
    let kernel_pages: u64 = k.segments[..k.count].iter().map(|s| s.pages).sum();
    1 + maps + 4 + kernel_pages.div_ceil(512) + 8
}

const DESC_CONVENTIONAL: u32 = 7;

fn kind(t: u32) -> MemKind {
    match t {
        1 | 2 | 3 | 4 | DESC_CONVENTIONAL => MemKind::Usable, // loader and boot-services memory is reclaimable
        5 => MemKind::RuntimeCode,
        6 => MemKind::RuntimeData,
        9 => MemKind::AcpiReclaim,
        10 => MemKind::AcpiNvs,
        11 | 12 => MemKind::Mmio,
        memtype::KERNEL | memtype::IMAGE => MemKind::Image,
        memtype::BOOT_DATA => MemKind::BootInfo,
        _ => MemKind::Reserved,
    }
}

/// Writes a line to COM1 directly (after `ExitBootServices` there's no console).
fn com1(s: &str) {
    if crate::FLAVOUR == Flavour::Release {
        return; // the release flavour has no serial log
    }
    for b in s.bytes() {
        // SAFETY: COM1's data and line-status ports; the firmware configured the UART.
        unsafe {
            let mut lsr: u8;
            loop {
                core::arch::asm!("in al, dx", out("al") lsr, in("dx") 0x3FDu16, options(nomem, nostack));
                if lsr & 0x20 != 0 {
                    break;
                }
            }
            core::arch::asm!("out dx, al", in("al") b, in("dx") 0x3F8u16, options(nomem, nostack));
        }
    }
}

/// Everything the handoff needs that isn't the kernel itself.
pub struct Facts {
    pub framebuffer: Framebuffer,
    pub edid: Option<(*const u8, usize)>,
    pub acpi_rsdp: u64,
    pub image: &'static [u8],
    pub version: [u8; 32],
    pub stamps: BootStamps,
    /// The slot booted (0 = A, 1 = B), and `FreallyBootCtl` as left this boot.
    pub slot: u8,
    pub bootctl: [u8; 32],
    /// Every linear GOP mode, `w << 16 | h` (BootInfo v3).
    pub gop_modes: [u32; fcore_abi::GOP_MODES_MAX],
    /// The firmware's vendor, in ASCII (BootInfo v3).
    pub firmware_vendor: [u8; 32],
    /// Boot options (dev and e2e: `\FREALLY\CMDLINE.TXT`; release: none).
    pub cmdline: [u8; 256],
}

/// The system table's vendor string (UCS-2) as ASCII, NUL-padded: anything
/// past ASCII becomes '?', and it stops at 31 characters.
///
/// # Safety
/// `p` must be null or the firmware's NUL-terminated vendor string.
pub unsafe fn vendor_ascii(p: *const u16) -> [u8; 32] {
    let mut out = [0u8; 32];
    if p.is_null() {
        return out;
    }
    for (i, slot) in out.iter_mut().enumerate().take(31) {
        // SAFETY: the string is NUL-terminated; we stop at its NUL.
        let c = unsafe { *p.add(i) };
        if c == 0 {
            break;
        }
        *slot = if (0x20..0x7F).contains(&c) { c as u8 } else { b'?' };
    }
    out
}

/// Exits boot services and jumps to Freally Core. Returns only if something
/// failed before `ExitBootServices` (after it, failure halts).
///
/// # Safety
/// Boot services must be running; `st` and `image_handle` are the firmware's.
pub unsafe fn handoff(st: &SystemTable, image_handle: Handle, k: &Kernel, facts: Facts) -> Why {
    // SAFETY: the firmware's boot services, still running.
    let bs = unsafe { &*st.boot_services };
    match unsafe { prepare_and_go(st, bs, image_handle, k, facts) } {
        Ok(never) => match never {},
        Err(why) => why,
    }
}

unsafe fn prepare_and_go(
    st: &SystemTable,
    bs: &BootServices,
    image_handle: Handle,
    k: &Kernel,
    f: Facts,
) -> Result<core::convert::Infallible, Why> {
    // 1. The memory map now, to size everything.
    let (mut size, mut key, mut desc, mut ver) = (0usize, 0usize, 0usize, 0u32);
    (bs.get_memory_map)(&mut size, null_mut(), &mut key, &mut desc, &mut ver);
    if desc == 0 {
        return Err("The firmware didn’t describe the computer’s memory.");
    }
    // Room for the map to grow while we allocate (each allocation can split a region).
    let map_cap = size + 64 * desc;
    let map_pages = (map_cap as u64).div_ceil(PAGE);
    let map_buf = alloc(bs, memtype::BOOT_DATA, map_pages)?;
    size = map_cap;
    if is_error((bs.get_memory_map)(&mut size, map_buf as *mut u8, &mut key, &mut desc, &mut ver)) {
        return Err("The firmware didn’t describe the computer’s memory.");
    }
    let mut max_phys = f.framebuffer.base + f.framebuffer.size;
    for i in 0..size / desc {
        // SAFETY: inside the map buffer the firmware just filled.
        let d = unsafe { (map_buf as *const u8).add(i * desc) };
        let (start, pages) = unsafe { (*(d.add(8) as *const u64), *(d.add(24) as *const u64)) };
        max_phys = max_phys.max(start + pages * PAGE);
    }

    // The Memory Attributes Table, copied now: it may live in memory boot
    // services own. Without one, runtime services stay off (they'd need
    // writable, executable pages, and W^X allows none).
    // SAFETY: boot services are running.
    let mat = unsafe { memory_attributes(st, bs) }?;
    // 2. Everything the kernel will be handed, allocated before the final map.
    // Each noted allocation can split a region in two more pieces.
    let regions_cap = size / desc + 64 + 2 * crate::ledger::MAX + mat.map_or(0, |m| m.1);
    let info_bytes = core::mem::size_of::<BootInfo>() as u64
        + (regions_cap * core::mem::size_of::<MemRegion>()) as u64
        + 512
        + (regions_cap * desc) as u64;
    let info_mem = alloc(bs, memtype::BOOT_DATA, info_bytes.div_ceil(PAGE))?;
    let info = info_mem as *mut BootInfo;
    let regions = (info_mem + core::mem::size_of::<BootInfo>() as u64).next_multiple_of(8) as *mut MemRegion;
    let edid_copy = (regions as u64 + (regions_cap * core::mem::size_of::<MemRegion>()) as u64) as *mut u8;
    let rt_descs = (edid_copy as u64 + 512).next_multiple_of(8) as *mut u8;
    let stack = alloc(bs, memtype::BOOT_DATA, BOOT_STACK / PAGE)?;
    let pool_n = pool_pages(max_phys, k);
    let pool_mem = alloc(bs, memtype::BOOT_DATA, pool_n)?;
    let mut pool = Pool { next: pool_mem, end: pool_mem + pool_n * PAGE };
    let pml4 = build_tables(&mut pool, max_phys, k)?;

    let edid = match f.edid {
        Some((ptr, n)) if n <= 512 => {
            // SAFETY: the firmware's EDID bytes, copied into our own memory.
            unsafe { core::ptr::copy_nonoverlapping(ptr, edid_copy, n) };
            Slice { ptr: (edid_copy as u64 + DIRECT_MAP) as *const u8, len: n as u64 }
        }
        _ => Slice { ptr: core::ptr::null(), len: 0 },
    };

    // 3. The final map, and ExitBootServices (retried once if the key went stale).
    let mut exited = false;
    for _ in 0..2 {
        size = map_cap;
        if is_error((bs.get_memory_map)(&mut size, map_buf as *mut u8, &mut key, &mut desc, &mut ver)) {
            return Err("The firmware didn’t describe the computer’s memory.");
        }
        if !is_error((bs.exit_boot_services)(image_handle, key)) {
            exited = true;
            break;
        }
    }
    if !exited {
        return Err("The firmware wouldn’t hand the computer over.");
    }
    // From here on: no boot services, no console. Failures halt.
    let mut stamps = f.stamps;
    // SAFETY: RDTSC has no side effects.
    stamps.b3 = unsafe { core::arch::x86_64::_rdtsc() };
    com1("freally-boot: boot services exited\r\n");

    // 4. The memory map, converted and coalesced.
    let mut n = 0usize;
    let mut rt_n = 0usize;
    for i in 0..size / desc {
        // SAFETY: inside the final map.
        let d = unsafe { (map_buf as *const u8).add(i * desc) };
        let (t, start, pages) = unsafe { (*(d as *const u32), *(d.add(8) as *const u64), *(d.add(24) as *const u64)) };
        let attr = unsafe { *(d.add(32) as *const u64) };
        let kd = kind(t);
        let len = pages * PAGE;
        // Runtime regions (EFI_MEMORY_RUNTIME) move into the direct map: every
        // one goes to SetVirtualAddressMap, merged into a region below or not.
        if attr & (1 << 63) != 0 {
            // SAFETY: `rt_descs` has room for every descriptor.
            unsafe {
                let out = rt_descs.add(rt_n * desc);
                core::ptr::copy_nonoverlapping(d, out, desc);
                *(out.add(16) as *mut u64) = start + DIRECT_MAP;
            }
            rt_n += 1;
        }
        // With a Memory Attributes Table, runtime code comes from it instead
        // (code and data apart), after this loop.
        if t == 5 && mat.is_some() {
            continue;
        }
        let mut push = |base: u64, len: u64, kd: MemKind| {
            // SAFETY: `regions` has room for every descriptor, each split by
            // the ledger, plus slack.
            unsafe {
                if n > 0 {
                    let last = &mut *regions.add(n - 1);
                    if last.kind == kd && last.base + last.len == base {
                        last.len += len;
                        return;
                    }
                }
                *regions.add(n) = MemRegion { base, len, kind: kd, _pad: 0 };
            }
            n += 1;
        };
        // The bootloader's own allocations are LoaderData to the firmware:
        // the ledger says which pages are the kernel's, the image's, the boot data's.
        if t == LOADER_DATA {
            crate::ledger::split(start, pages, |s, p, tag| push(s, p * PAGE, kind(tag.unwrap_or(LOADER_DATA))));
        } else {
            push(start, len, kd);
        }
    }
    // The runtime images, as the table splits them: read-only code (the kernel
    // maps it executable) and no-execute data.
    if let Some((buf, count, size)) = mat {
        for i in 0..count {
            // SAFETY: our copy of the table: `count` descriptors of `size` bytes.
            let (t, start, pages, attr) = unsafe {
                let d = (buf as *const u8).add(16 + i * size);
                (*(d as *const u32), *(d.add(8) as *const u64), *(d.add(24) as *const u64), *(d.add(32) as *const u64))
            };
            if t != 5 {
                continue;
            }
            let code = attr & MEMORY_RO != 0 && attr & MEMORY_XP == 0;
            let kd = if code { MemKind::RuntimeCode } else { MemKind::RuntimeData };
            // SAFETY: `regions` has room for the table's entries too.
            unsafe { *regions.add(n) = MemRegion { base: start, len: pages * PAGE, kind: kd, _pad: 0 } };
            n += 1;
        }
    }

    // 5. The runtime services, moved to the direct map.
    // SAFETY: called once, after ExitBootServices, with the runtime descriptors.
    let rt = st.runtime_services;
    let moved = !rt.is_null()
        && rt_n > 0
        && !is_error(unsafe { ((*rt).set_virtual_address_map)(rt_n * desc, desc, ver, rt_descs) });
    com1(if !moved {
        "freally-boot: runtime services not moved\r\n"
    } else if mat.is_none() {
        "freally-boot: runtime services moved, but off: no memory attributes table (W^X)\r\n"
    } else {
        "freally-boot: runtime services moved\r\n"
    });

    // 6. BootInfo, at its direct-map address.
    let bi = BootInfo {
        magic: fcore_abi::BOOTINFO_MAGIC,
        version: fcore_abi::BOOTINFO_VERSION,
        flavour: crate::FLAVOUR as u32,
        memory_map: Slice { ptr: (regions as u64 + DIRECT_MAP) as *const MemRegion, len: n as u64 },
        framebuffer: f.framebuffer,
        edid,
        acpi_rsdp: f.acpi_rsdp,
        // The table lives in runtime memory, which SetVirtualAddressMap moved
        // to its direct-map address. (Not `st.runtime_services` reread: the
        // firmware rewrote it behind a shared reference, so the compiler may
        // hand back the value it read before.) Only with the attributes
        // table: without it, runtime code can't be mapped W^X.
        uefi_runtime: if moved && mat.is_some() { rt as u64 + DIRECT_MAP } else { 0 },
        image: ImageLoc {
            base: f.image.as_ptr() as u64,
            len: f.image.len() as u64,
            slot: f.slot as u32,
            _pad: 0,
            version: f.version,
        },
        kernel: k.loc,
        tsc: stamps,
        cmdline: f.cmdline,
        bootctl: f.bootctl,
        gop_modes: f.gop_modes,
        firmware_vendor: f.firmware_vendor,
    };
    // SAFETY: `info` is our BOOT_DATA page.
    unsafe { info.write(bi) };

    // 7. The jump: no-execute on, the new tables, the boot stack, BootInfo in RDI.
    com1("freally-boot: jumping to Freally Core\r\n");
    let stack_top = stack + DIRECT_MAP + BOOT_STACK - 8; // as if `call` pushed a return address
    // SAFETY: the tables map this code (identity), the stack and BootInfo (direct
    // map) and the kernel; EFER.NXE must be on before tables with NX bits load.
    unsafe {
        core::arch::asm!(
            // No interrupts until the kernel has its own IDT: the firmware's timer
            // could otherwise land on a vector with no gate.
            "cli",
            "mov ecx, 0xC0000080",
            "rdmsr",
            "or eax, 0x800",
            "wrmsr",
            "mov cr3, r8",
            "mov rsp, r9",
            "xor ebp, ebp",
            "jmp r10",
            // Pinned away from rax, rcx and rdx, which rdmsr and wrmsr use.
            in("r8") pml4,
            in("r9") stack_top,
            in("r10") k.entry,
            in("rdi") info as u64 + DIRECT_MAP,
            options(noreturn)
        )
    }
}

/// The framebuffer as the kernel sees it, from the GOP mode.
pub fn framebuffer(d: &crate::video::Display, base: u64, size: u64) -> Framebuffer {
    Framebuffer {
        base,
        size,
        width: d.w as u32,
        height: d.h as u32,
        stride_px: d.stride as u32,
        format: if d.format == 0 { PixelFormat::Rgbx8888 } else { PixelFormat::Bgrx8888 },
    }
}

/// The firmware's Memory Attributes Table, copied into a page of our own:
/// (the copy, its entry count, its descriptor size). None if there's none.
///
/// # Safety
/// Boot services are running.
unsafe fn memory_attributes(st: &SystemTable, bs: &BootServices) -> Result<Option<(u64, usize, usize)>, Why> {
    // SAFETY: the firmware's configuration table has `number_of_table_entries` entries.
    let tables = unsafe { core::slice::from_raw_parts(st.configuration_table, st.number_of_table_entries) };
    let Some(t) = tables.iter().find(|t| t.vendor_guid == MEMORY_ATTRIBUTES_TABLE_GUID) else { return Ok(None) };
    let m = t.vendor_table as *const u8;
    // SAFETY: the table's header: version, entry count, descriptor size, flags (u32 each).
    let (count, size) = unsafe { (*(m.add(4) as *const u32) as usize, *(m.add(8) as *const u32) as usize) };
    if size < 40 || count > 4096 {
        return Ok(None);
    }
    let bytes = 16 + count * size;
    let copy = alloc(bs, memtype::BOOT_DATA, (bytes as u64).div_ceil(PAGE))?;
    // SAFETY: `bytes` bytes of the firmware's table into our pages.
    unsafe { core::ptr::copy_nonoverlapping(m, copy as *mut u8, bytes) };
    Ok(Some((copy, count, size)))
}
