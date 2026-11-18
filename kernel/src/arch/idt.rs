// SPDX-License-Identifier: Apache-2.0
//! The IDT (TASK-019): every exception (0–31), the interrupt vectors the APIC
//! uses, and the spurious vector. Every entry runs the same assembly stub,
//! which saves the registers into a [`Frame`] and calls [`dispatch`].

use super::gdt;
use core::mem::size_of;

/// The registers saved on entry, then what the CPU pushed.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

impl Frame {
    pub fn is_user(&self) -> bool {
        self.cs & 3 == 3
    }
}

macro_rules! stubs {
    ($($n:literal)*) => {
        core::arch::global_asm!(
            $(
                concat!("isr_", $n, ":"),
                // The CPU pushes an error code for 8, 10–14, 17, 21 and 29; the
                // others get a 0, so every frame has the same shape.
                concat!(".if ", $n, " == 8 || (", $n, " >= 10 && ", $n, " <= 14) || ", $n, " == 17 || ", $n, " == 21 || ", $n, " == 29"),
                ".else",
                "    push 0",
                ".endif",
                concat!("    push ", $n),
                "    jmp isr_common",
            )*
            ".section .rodata",
            ".p2align 3",
            ".global ISR_TABLE",
            "ISR_TABLE:",
            $( concat!("    .quad isr_", $n), )*
            ".text",
        );
    };
}

// 0–31 exceptions, 32–47 the APIC's vectors (timer, IPIs, IRQs), 255 spurious.
stubs!(0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31
       32 33 34 35 36 37 38 39 40 41 42 43 44 45 46 47 255);

unsafe extern "C" {
    /// The stubs' addresses, in the order `stubs!` lists them.
    static ISR_TABLE: [u64; 49];
}

core::arch::global_asm!(
    "isr_common:",
    // From ring 3: swap to the kernel's GS (the per-CPU data) on the way in and back on the way out.
    "    test qword ptr [rsp + 24], 3",
    "    jz 1f",
    "    swapgs",
    "1:",
    "    push rax",
    "    push rbx",
    "    push rcx",
    "    push rdx",
    "    push rsi",
    "    push rdi",
    "    push rbp",
    "    push r8",
    "    push r9",
    "    push r10",
    "    push r11",
    "    push r12",
    "    push r13",
    "    push r14",
    "    push r15",
    "    mov rdi, rsp",
    "    cld",
    "    call {dispatch}",
    "    pop r15",
    "    pop r14",
    "    pop r13",
    "    pop r12",
    "    pop r11",
    "    pop r10",
    "    pop r9",
    "    pop r8",
    "    pop rbp",
    "    pop rdi",
    "    pop rsi",
    "    pop rdx",
    "    pop rcx",
    "    pop rbx",
    "    pop rax",
    "    test qword ptr [rsp + 24], 3",
    "    jz 2f",
    "    swapgs",
    "2:",
    "    add rsp, 16",
    "    iretq",
    dispatch = sym dispatch,
);

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Gate {
    off_lo: u16,
    sel: u16,
    ist: u8,
    attr: u8,
    off_mid: u16,
    off_hi: u32,
    _r: u32,
}

static mut IDT: [Gate; 256] = [Gate { off_lo: 0, sel: 0, ist: 0, attr: 0, off_mid: 0, off_hi: 0, _r: 0 }; 256];

fn gate(handler: u64, ist: u8) -> Gate {
    Gate {
        off_lo: handler as u16,
        sel: gdt::KERNEL_CS,
        ist,
        attr: 0x8E, // present, ring 0, 64-bit interrupt gate (interrupts off on entry)
        off_mid: (handler >> 16) as u16,
        off_hi: (handler >> 32) as u32,
        _r: 0,
    }
}

/// Fills and loads the IDT.
pub fn init() {
    let vectors: [u8; 49] = {
        let mut v = [0u8; 49];
        let mut i = 0;
        while i < 48 {
            v[i] = i as u8;
            i += 1;
        }
        v[48] = 255;
        v
    };
    // SAFETY: called once before interrupts are on.
    unsafe {
        let idt = &raw mut IDT;
        for (i, &v) in vectors.iter().enumerate() {
            let ist = match v {
                8 => gdt::IST_DOUBLE_FAULT,
                2 => gdt::IST_NMI,
                18 => gdt::IST_MACHINE_CHECK,
                _ => 0,
            };
            (*idt)[v as usize] = gate(ISR_TABLE[i], ist);
        }
        #[repr(C, packed)]
        struct Pointer {
            limit: u16,
            base: u64,
        }
        let ptr = Pointer { limit: (size_of::<[Gate; 256]>() - 1) as u16, base: idt as u64 };
        core::arch::asm!("lidt [{}]", in(reg) &ptr, options(readonly, nostack, preserves_flags));
    }
}

/// The handler a vector ≥ 32 is routed to.
pub type IrqHandler = fn(&mut Frame);
static mut IRQ: [Option<IrqHandler>; 256] = [None; 256];

/// Routes `vector` (32–47 or 255) to `h`.
pub fn set_handler(vector: u8, h: IrqHandler) {
    // SAFETY: written before the vector is unmasked; read only in dispatch.
    unsafe { IRQ[vector as usize] = Some(h) };
}

extern "C" fn dispatch(f: &mut Frame) {
    if f.vector < 32 {
        crate::exceptions::exception(f);
        return;
    }
    // SAFETY: the table is filled before vectors are unmasked.
    let h = unsafe { IRQ[f.vector as usize] };
    match h {
        Some(h) => h(f),
        None => crate::exceptions::unexpected_interrupt(f),
    }
}
