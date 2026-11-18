// SPDX-License-Identifier: Apache-2.0
//! The GDT and the TSS (TASK-019), one of each per CPU (TASK-036).
//!
//! Selectors are laid out for `SYSCALL`/`SYSRET` (TASK-027): kernel code 0x08,
//! kernel data 0x10, user data 0x18 (|3), user code 0x20 (|3), the TSS at 0x28.
//! The TSS's interrupt stacks give #DF, NMI and #MC stacks of their own, so a
//! kernel stack overflow still reaches the panic screen. Each CPU has its own
//! TSS (its kernel stack, its I/O bitmap, its interrupt stacks), so each has
//! its own GDT to point at it.

use core::mem::size_of;

pub const KERNEL_CS: u16 = 0x08;
pub const KERNEL_DS: u16 = 0x10;
pub const USER_DS: u16 = 0x18 | 3;
pub const USER_CS: u16 = 0x20 | 3;
const TSS_SEL: u16 = 0x28;

pub const IST_DOUBLE_FAULT: u8 = 1;
pub const IST_NMI: u8 = 2;
pub const IST_MACHINE_CHECK: u8 = 3;

const STACK_SIZE: usize = 16 * 1024;

#[repr(C, align(16))]
struct Stack([u8; STACK_SIZE]);

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

/// The TSS and its I/O permission bitmap (a set bit denies the port; the
/// final byte must be all ones). Every port is closed until a process with
/// granted ports runs (`hw::io_switch`).
#[repr(C, packed)]
struct TssIo {
    tss: Tss,
    map: [u8; 8192 + 1],
}

/// One CPU's GDT and TSS.
#[repr(C, align(16))]
pub struct Tables {
    gdt: [u64; 7],
    tss: TssIo,
}

impl Tables {
    const fn new() -> Tables {
        Tables {
            gdt: [
                0,
                0x00AF_9A00_0000_FFFF, // kernel code: present, ring 0, executable, long mode
                0x00CF_9200_0000_FFFF, // kernel data
                0x00CF_F200_0000_FFFF, // user data, ring 3
                0x00AF_FA00_0000_FFFF, // user code, ring 3, long mode
                0,                     // TSS, low half (filled in at load)
                0,                     // TSS, high half
            ],
            tss: TssIo {
                tss: Tss {
                    _r0: 0,
                    rsp: [0; 3],
                    _r1: 0,
                    ist: [0; 7],
                    _r2: 0,
                    _r3: 0,
                    iomap_base: size_of::<Tss>() as u16,
                },
                map: [0xFF; 8192 + 1],
            },
        }
    }
}

/// The boot CPU's tables and its four stacks (#DF, NMI, #MC, and the one an
/// interrupt from user mode lands on before any thread has its own).
static mut BSP: Tables = Tables::new();
static mut BSP_STACKS: [Stack; 4] = [const { Stack([0; STACK_SIZE]) }; 4];

/// A CPU's tables and the tops of its four stacks.
pub struct CpuTables {
    pub tables: *mut Tables,
    pub stacks: [u64; 4],
}

/// The boot CPU's.
pub fn bsp() -> CpuTables {
    let s = &raw mut BSP_STACKS;
    // SAFETY: only addresses are taken; the stacks are 16 KiB each.
    let top = |i: usize| unsafe { (&raw mut (*s)[i]) as u64 + STACK_SIZE as u64 };
    CpuTables { tables: &raw mut BSP, stacks: [top(0), top(1), top(2), top(3)] }
}

/// Another CPU's, from the heap; never freed (a CPU never goes away).
pub fn alloc() -> Result<CpuTables, &'static str> {
    use alloc::alloc::{Layout, alloc_zeroed};
    let tl = Layout::new::<Tables>();
    let sl = Layout::new::<[Stack; 4]>();
    // SAFETY: both layouts have a non-zero size.
    let (t, s) = unsafe { (alloc_zeroed(tl) as *mut Tables, alloc_zeroed(sl) as *mut [Stack; 4]) };
    if t.is_null() || s.is_null() {
        return Err("out of memory for a CPU's tables");
    }
    // SAFETY: `t` is a fresh allocation of the right layout.
    unsafe { t.write(Tables::new()) };
    let top = |i: usize| s as u64 + (i * STACK_SIZE + STACK_SIZE) as u64;
    Ok(CpuTables { tables: t, stacks: [top(0), top(1), top(2), top(3)] })
}

/// Loads a CPU's GDT and TSS on the CPU it belongs to, and reloads every
/// segment register.
///
/// # Safety
/// Once per CPU, before interrupts are on; `c` is this CPU's and lives forever.
pub unsafe fn load(c: &CpuTables) {
    // SAFETY: the tables are this CPU's alone; nothing else touches them yet.
    unsafe {
        let tss = &raw mut (*c.tables).tss.tss;
        for (i, s) in [IST_DOUBLE_FAULT, IST_NMI, IST_MACHINE_CHECK].iter().enumerate() {
            (*tss).ist[*s as usize - 1] = c.stacks[i];
        }
        (*tss).rsp[0] = c.stacks[3];
        let base = tss as u64;
        let limit = (size_of::<TssIo>() - 1) as u64;
        let gdt = &raw mut (*c.tables).gdt;
        (*gdt)[5] = (limit & 0xFFFF)
            | ((base & 0xFF_FFFF) << 16)
            | (0x89 << 40) // present, 64-bit TSS (available)
            | (((limit >> 16) & 0xF) << 48)
            | (((base >> 24) & 0xFF) << 56);
        (*gdt)[6] = base >> 32;
        #[repr(C, packed)]
        struct Pointer {
            limit: u16,
            base: u64,
        }
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

/// This CPU's TSS.
fn this_tss() -> *mut TssIo {
    // SAFETY: each CPU's block points at its own tables, which live forever.
    unsafe { &raw mut (*crate::smp::this().tables.load(core::sync::atomic::Ordering::Relaxed)).tss }
}

/// Opens (or closes) `len` ports from `port` in this CPU's I/O bitmap.
/// Interrupts off: the bitmap is this CPU's.
pub fn io_set(port: u16, len: u16, open: bool) {
    let t = this_tss();
    for p in port as usize..(port as usize + len as usize).min(0x1_0000) {
        // SAFETY: this CPU's bitmap; the caller has interrupts off.
        let byte = unsafe { &mut (*t).map[p / 8] };
        if open {
            *byte &= !(1 << (p % 8));
        } else {
            *byte |= 1 << (p % 8);
        }
    }
}

/// Sets the stack this CPU switches to when an interrupt or system call
/// arrives from user mode (the current thread's kernel stack). Interrupts off.
pub fn set_kernel_stack(top: u64) {
    let t = this_tss();
    // SAFETY: a single store into this CPU's TSS; the CPU reads it only on a ring change.
    unsafe { core::ptr::write_unaligned(&raw mut (*t).tss.rsp[0], top) };
}
