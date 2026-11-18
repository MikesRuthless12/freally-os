// SPDX-License-Identifier: Apache-2.0
//! The AP trampoline (TASK-036): where a SIPI starts an AP, in real mode, at
//! a page below 1 MiB. It goes straight to long mode:
//!
//! 1. CR4 gets PAE and PGE; CR3 the trampoline's own PML4 (below 1 MiB, so
//!    real mode's 32-bit CR3 can hold it); EFER gets LME, NXE and SCE.
//! 2. Its GDT (64-bit code at 0x08, data at 0x10), then CR0 with PE, PG, WP
//!    and NE on and the caches on (an AP comes out of INIT with CD and NW set).
//! 3. A far jump through a pointer to its 64-bit half, at its physical
//!    address: the trampoline's PML4 maps the low 2 MiB one to one, and the
//!    kernel's upper half as the kernel's own tables do.
//! 4. `lock xadd` takes the next slot of the table: a boot stack and a CPU
//!    block; it calls the entry with the block.
//!
//! The code below 1 MiB is a copy, so it uses no absolute address of its own:
//! the real-mode half reads its data relative to CS, the long-mode half
//! relative to RIP, and the three absolute values (the GDT's base, the far
//! pointer's offset, CR3) are written into the copy.
//!
//! The low 2 MiB mapping is writable and executable, which W^X otherwise
//! forbids: it lives only in the trampoline's own tables, which only APs on
//! their way in ever load.

use crate::mm::frames::FRAME;
use crate::mm::paging;
use fcore_abi::MemKind;
use fcore_abi::layout::DIRECT_MAP;

core::arch::global_asm!(
    ".pushsection .rodata.ap_trampoline, \"a\"",
    ".balign 16",
    ".global ap_tramp_start",
    "ap_tramp_start:",
    ".code16",
    "    cli",
    "    cld",
    "    mov ax, cs",
    "    mov ds, ax",
    "    mov eax, 0xA0", // CR4: PAE | PGE
    "    mov cr4, eax",
    // mov eax, dword [ap_tramp_cr3] (relative to CS, by hand: LLVM takes no label difference here)
    "    .byte 0x66, 0xA1",
    "    .word ap_tramp_cr3 - ap_tramp_start",
    "    mov cr3, eax",
    "    mov ecx, 0xC0000080", // EFER: SCE | LME | NXE
    "    rdmsr",
    "    or eax, 0x901",
    "    wrmsr",
    // lgdt [ap_tramp_gdtr]
    "    .byte 0x0F, 0x01, 0x16",
    "    .word ap_tramp_gdtr - ap_tramp_start",
    "    mov eax, cr0",
    "    and eax, 0x9FFFFFFF", // caches on: CD and NW off
    "    or eax, 0x80010021",  // PG | WP | NE | PE
    "    mov cr0, eax",
    // jmp far dword [ap_tramp_far]: into the 64-bit half, at its physical address.
    "    .byte 0x66, 0xFF, 0x2E",
    "    .word ap_tramp_far - ap_tramp_start",
    ".code64",
    ".global ap_tramp_long",
    "ap_tramp_long:",
    "    mov ax, 0x10",
    "    mov ds, ax",
    "    mov es, ax",
    "    mov ss, ax",
    "    mov eax, 1",
    "    lock xadd dword ptr [rip + ap_tramp_count], eax",
    "    shl rax, 4",
    "    lea rbx, [rip + ap_tramp_table]",
    "    mov rsp, qword ptr [rbx + rax]",
    "    mov rdi, qword ptr [rbx + rax + 8]",
    "    mov rax, qword ptr [rip + ap_tramp_entry]",
    "    call rax",
    "    ud2",
    ".balign 8",
    ".global ap_tramp_far",
    "ap_tramp_far:",
    "    .long 0", // the 64-bit half's physical address (written)
    "    .word 0x08",
    ".balign 8",
    ".global ap_tramp_gdtr",
    "ap_tramp_gdtr:",
    "    .word 23",
    "    .long 0", // the GDT's physical address (written)
    ".balign 8",
    ".global ap_tramp_gdt",
    "ap_tramp_gdt:",
    "    .quad 0",
    "    .quad 0x00AF9A000000FFFF",
    "    .quad 0x00CF92000000FFFF",
    ".global ap_tramp_cr3",
    "ap_tramp_cr3:",
    "    .quad 0",
    ".global ap_tramp_entry",
    "ap_tramp_entry:",
    "    .quad 0",
    ".global ap_tramp_count",
    "ap_tramp_count:",
    "    .quad 0",
    ".balign 16",
    ".global ap_tramp_table",
    "ap_tramp_table:",
    "    .space 16 * 64",
    ".global ap_tramp_end",
    "ap_tramp_end:",
    ".popsection",
);

unsafe extern "C" {
    static ap_tramp_start: u8;
    static ap_tramp_long: u8;
    static ap_tramp_far: u8;
    static ap_tramp_gdtr: u8;
    static ap_tramp_gdt: u8;
    static ap_tramp_cr3: u8;
    static ap_tramp_entry: u8;
    static ap_tramp_table: u8;
    static ap_tramp_end: u8;
}

/// Where a label sits in the trampoline.
fn off(label: *const u8) -> u64 {
    label as u64 - &raw const ap_tramp_start as u64
}

pub struct Trampoline {
    /// The code page's physical address.
    code: u64,
}

/// `n` usable pages below 1 MiB, the first page (the real-mode IVT) left alone.
fn low_pages<const N: usize>() -> Option<[u64; N]> {
    let bi = crate::boot_info()?;
    // SAFETY: the map lives in BOOT_DATA memory the kernel never frees.
    let map = unsafe { core::slice::from_raw_parts(bi.memory_map.ptr, bi.memory_map.len as usize) };
    let mut out = [0u64; N];
    let mut got = 0;
    for r in map.iter().filter(|r| r.kind == MemKind::Usable && r.base < 0x10_0000) {
        let mut p = r.base.next_multiple_of(FRAME).max(FRAME);
        while p + FRAME <= (r.base + r.len).min(0x10_0000) && got < N {
            out[got] = p;
            got += 1;
            p += FRAME;
        }
    }
    (got == N).then_some(out)
}

fn put<T>(phys: u64, at: u64, v: T) {
    // SAFETY: a page below 1 MiB the trampoline owns, through the direct map.
    unsafe { core::ptr::write_unaligned((phys + DIRECT_MAP + at) as *mut T, v) };
}

impl Trampoline {
    /// Copies the trampoline below 1 MiB and builds its page tables; APs
    /// will call `entry` with their block.
    pub fn prepare(entry: extern "C" fn(&'static super::Cpu) -> !) -> Result<Trampoline, &'static str> {
        // The frame allocator never hands out memory below 1 MiB: these pages are ours.
        let [code, p4, p3, p2] = low_pages::<4>().ok_or("no four usable pages below 1 MiB for the AP trampoline")?;
        let len = off(&raw const ap_tramp_end) as usize;
        if len > FRAME as usize {
            return Err("the AP trampoline is over a page");
        }
        // SAFETY: the blob is `len` bytes of the kernel's rodata; the page is ours.
        unsafe {
            core::ptr::copy_nonoverlapping(&raw const ap_tramp_start, (code + DIRECT_MAP) as *mut u8, len);
            for t in [p4, p3, p2] {
                core::ptr::write_bytes((t + DIRECT_MAP) as *mut u8, 0, FRAME as usize);
            }
            // The kernel's half, as the kernel's own tables have it.
            let k = (paging::kernel_pml4() + DIRECT_MAP) as *const u64;
            let d = (p4 + DIRECT_MAP) as *mut u64;
            core::ptr::copy_nonoverlapping(k.add(256), d.add(256), 256);
        }
        // The low 2 MiB, one to one: PML4[0] → PDPT[0] → a 2 MiB page at 0.
        put(p4, 0, p3 | paging::PRESENT | paging::WRITABLE);
        put(p3, 0, p2 | paging::PRESENT | paging::WRITABLE);
        put(p2, 0, paging::PRESENT | paging::WRITABLE | paging::HUGE);
        put(code, off(&raw const ap_tramp_far), (code + off(&raw const ap_tramp_long)) as u32);
        put(code, off(&raw const ap_tramp_gdtr) + 2, (code + off(&raw const ap_tramp_gdt)) as u32);
        put(code, off(&raw const ap_tramp_cr3), p4);
        put(code, off(&raw const ap_tramp_entry), entry as *const () as u64);
        Ok(Trampoline { code })
    }

    /// The SIPI's vector: the code page's number.
    pub fn page(&self) -> u8 {
        (self.code / FRAME) as u8
    }

    /// The `slot`th AP to arrive gets this stack and block.
    pub fn set_slot(&self, slot: usize, stack_top: u64, block: u64) {
        let at = off(&raw const ap_tramp_table) + slot as u64 * 16;
        put(self.code, at, stack_top);
        put(self.code, at + 8, block);
    }
}
