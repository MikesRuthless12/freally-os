// SPDX-License-Identifier: Apache-2.0
//! The GDT and the TSS (TASK-019).
//!
//! Selectors are laid out for `SYSCALL`/`SYSRET` (TASK-027): kernel code 0x08,
//! kernel data 0x10, user data 0x18 (|3), user code 0x20 (|3), the TSS at 0x28.
//! The TSS's interrupt stacks give #DF, NMI and #MC stacks of their own, so a
//! kernel stack overflow still reaches the panic screen.

use core::mem::size_of;

pub const KERNEL_CS: u16 = 0x08;
pub const KERNEL_DS: u16 = 0x10;
pub const USER_DS: u16 = 0x18 | 3;
pub const USER_CS: u16 = 0x20 | 3;
const TSS_SEL: u16 = 0x28;

pub const IST_DOUBLE_FAULT: u8 = 1;
pub const IST_NMI: u8 = 2;
pub const IST_MACHINE_CHECK: u8 = 3;

const IST_SIZE: usize = 16 * 1024;

#[repr(C, align(16))]
struct Stack([u8; IST_SIZE]);

static mut IST_STACKS: [Stack; 3] = [const { Stack([0; IST_SIZE]) }; 3];
static mut KERNEL_ENTRY_STACK: Stack = Stack([0; IST_SIZE]);

#[repr(C, packed)]
pub struct Tss {
    _r0: u32,
    pub rsp: [u64; 3],
    _r1: u64,
    pub ist: [u64; 7],
    _r2: u64,
    _r3: u16,
    iomap_base: u16,
}

static mut TSS: Tss =
    Tss { _r0: 0, rsp: [0; 3], _r1: 0, ist: [0; 7], _r2: 0, _r3: 0, iomap_base: size_of::<Tss>() as u16 };

static mut GDT: [u64; 7] = [
    0,
    0x00AF_9A00_0000_FFFF, // kernel code: present, ring 0, executable, long mode
    0x00CF_9200_0000_FFFF, // kernel data
    0x00CF_F200_0000_FFFF, // user data, ring 3
    0x00AF_FA00_0000_FFFF, // user code, ring 3, long mode
    0,                     // TSS, low half (filled in at init)
    0,                     // TSS, high half
];

#[repr(C, packed)]
struct Pointer {
    limit: u16,
    base: u64,
}

fn stack_top(s: *const Stack) -> u64 {
    s as u64 + IST_SIZE as u64
}

/// Loads the GDT and the TSS, and reloads every segment register.
pub fn init() {
    // SAFETY: called once on the boot CPU before interrupts are on; nothing
    // else touches these statics yet.
    unsafe {
        let tss = &raw mut TSS;
        for (i, s) in [IST_DOUBLE_FAULT, IST_NMI, IST_MACHINE_CHECK].iter().enumerate() {
            (*tss).ist[*s as usize - 1] = stack_top(&raw const IST_STACKS[i]);
        }
        (*tss).rsp[0] = stack_top(&raw const KERNEL_ENTRY_STACK);
        let base = tss as u64;
        let limit = (size_of::<Tss>() - 1) as u64;
        let gdt = &raw mut GDT;
        (*gdt)[5] = (limit & 0xFFFF)
            | ((base & 0xFF_FFFF) << 16)
            | (0x89 << 40) // present, 64-bit TSS (available)
            | (((limit >> 16) & 0xF) << 48)
            | (((base >> 24) & 0xFF) << 56);
        (*gdt)[6] = base >> 32;
        let ptr = Pointer { limit: (size_of::<[u64; 7]>() - 1) as u16, base: gdt as u64 };
        core::arch::asm!(
            "lgdt [{ptr}]",
            // Reload CS with a far return, then the data segments.
            "push {cs}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            "mov ds, {ds:x}",
            "mov es, {ds:x}",
            "mov ss, {ds:x}",
            "xor {tmp:e}, {tmp:e}",
            "mov fs, {tmp:x}",
            "mov gs, {tmp:x}",
            "ltr {tss:x}",
            ptr = in(reg) &ptr,
            cs = in(reg) KERNEL_CS as u64,
            ds = in(reg) KERNEL_DS as u64,
            tss = in(reg) TSS_SEL as u64,
            tmp = out(reg) _,
        );
    }
}

/// Sets the stack the CPU switches to when an interrupt or system call
/// arrives from user mode (the current thread's kernel stack).
pub fn set_kernel_stack(top: u64) {
    // SAFETY: a single aligned store; the CPU reads it only on a ring change.
    unsafe {
        let tss = &raw mut TSS;
        core::ptr::write_unaligned(&raw mut (*tss).rsp[0], top);
    }
}
