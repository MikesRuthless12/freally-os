// SPDX-License-Identifier: Apache-2.0
//! Exceptions and the panic screen (TASK-019, FR-005).
//!
//! A fault in the kernel stops the machine on the panic screen: what happened,
//! where (RIP; for a page fault, the address), drawn in Noto from the boot atlas
//! straight onto the framebuffer, and logged on COM1. Faults from user mode
//! become fault reports instead (TASK-031).
//!
//! **The exception table:** an instruction listed in `.extable` that faults
//! resumes at its fix-up instead of panicking. The probes below (and, from
//! TASK-025, the user-copy routines) use it, so a bad address is an error, not a crash.

use crate::arch::idt::Frame;
use crate::arch::{cpu, idt};
use crate::fmtbuf::Buf;
use core::sync::atomic::{AtomicU64, Ordering};

/// The vector the last fixed-up fault had (for tests and user copies).
pub static LAST_FIXED_VECTOR: AtomicU64 = AtomicU64::new(u64::MAX);

pub fn name(vector: u64) -> &'static str {
    match vector {
        0 => "A division by zero (#DE)",
        1 => "A debug trap (#DB)",
        2 => "A non-maskable interrupt (NMI)",
        3 => "A breakpoint (#BP)",
        4 => "An overflow (#OF)",
        5 => "A bound-range fault (#BR)",
        6 => "An invalid instruction (#UD)",
        7 => "A missing FPU (#NM)",
        8 => "A double fault (#DF)",
        10 => "An invalid TSS (#TS)",
        11 => "A missing segment (#NP)",
        12 => "A stack fault (#SS)",
        13 => "A general protection fault (#GP)",
        14 => "A page fault (#PF)",
        16 => "An x87 error (#MF)",
        17 => "An alignment fault (#AC)",
        18 => "A machine check (#MC)",
        19 => "A SIMD error (#XM)",
        20 => "A virtualization exception (#VE)",
        21 => "A control-protection fault (#CP)",
        _ => "An unexpected exception",
    }
}

unsafe extern "C" {
    static __extable_start: [[u64; 2]; 0];
    static __extable_end: [[u64; 2]; 0];
}

/// The fix-up for a faulting `rip`, if the exception table lists it.
fn fixup(rip: u64) -> Option<u64> {
    // SAFETY: the linker places `.extable` between these symbols: pairs of
    // (faulting instruction, fix-up), both kernel text addresses.
    unsafe {
        let mut p = (&raw const __extable_start) as *const [u64; 2];
        let end = (&raw const __extable_end) as *const [u64; 2];
        while p < end {
            if (*p)[0] == rip {
                return Some((*p)[1]);
            }
            p = p.add(1);
        }
    }
    None
}

/// Armed by `try_exec`: an instruction-fetch page fault resumes at the probe's fix-up.
static EXEC_PROBE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub fn exception(f: &mut Frame) {
    // NMIs carry shootdowns and the panic stop (TASK-036), from user mode too.
    if f.vector == 2 {
        crate::smp::on_nmi();
        return;
    }
    if f.is_user() {
        crate::fault::user_fault(f);
        return;
    }
    if f.vector == 14 && f.error & 0x10 != 0 && EXEC_PROBE.swap(false, Ordering::SeqCst) {
        LAST_FIXED_VECTOR.store(14, Ordering::Relaxed);
        f.rip = probe_exec_fix as *const () as u64;
        return;
    }
    if let Some(to) = fixup(f.rip) {
        LAST_FIXED_VECTOR.store(f.vector, Ordering::Relaxed);
        f.rip = to;
        return;
    }
    let mut what = Buf::<160>::new();
    let _ = core::fmt::write(&mut what, format_args!("{} in Freally Core at {:#x}", name(f.vector), f.rip));
    if f.vector == 14 {
        let _ = core::fmt::write(&mut what, format_args!(", touching {:#x}", cpu::cr2()));
    }
    let mut detail = Buf::<160>::new();
    let _ = core::fmt::write(
        &mut detail,
        format_args!("Error code {:#x}, stack {:#x}, flags {:#x}.", f.error, f.rsp, f.rflags),
    );
    stop(what.as_str(), detail.as_str());
}

pub fn unexpected_interrupt(f: &mut Frame) {
    klog!("freally-core: an interrupt arrived on vector {} with no handler", f.vector);
    // Taken all the same: left in service, it would hold off this CPU's
    // timer and every IPI at or below its priority for good (the spurious
    // vector, which takes no EOI, has its own handler).
    crate::arch::apic::eoi();
}

crate::ktest!(exceptions_a_stray_interrupt_leaves_the_timer_working, {
    use crate::arch::apic;
    // A vector nothing routes: between the reschedule IPI (33) and the
    // device lines (48 on). Its priority class (2) is the timer's: left in
    // service, it would hold the timer and every IPI off this CPU for good.
    const STRAY: u8 = 40;
    let c = crate::smp::this();
    let was_on = cpu::interrupts_enabled();
    cpu::interrupts_off();
    apic::send_ipi(apic::id(), STRAY);
    cpu::interrupts_on();
    let until = cpu::rdtsc() + crate::time::ns_to_ticks(200_000);
    while cpu::rdtsc() < until {
        core::hint::spin_loop();
    }
    cpu::interrupts_off();
    let before = c.fires.load(Ordering::SeqCst);
    apic::arm_deadline(cpu::rdtsc() + crate::time::ns_to_ticks(1_000_000));
    cpu::interrupts_on();
    let give_up = cpu::rdtsc() + crate::time::ns_to_ticks(200_000_000);
    while c.fires.load(Ordering::SeqCst) == before && cpu::rdtsc() < give_up {
        core::hint::spin_loop();
    }
    cpu::interrupts_off();
    let fired = c.fires.load(Ordering::SeqCst) != before;
    if !fired {
        // Put this CPU right for the tests after this one.
        apic::eoi();
    }
    if was_on {
        cpu::interrupts_on();
    }
    if fired {
        Ok(())
    } else {
        Err("after a stray interrupt the timer never fired: the stray vector was left in service")
    }
});

/// Logs, paints the panic screen and halts. Never returns.
pub fn stop(what: &str, detail: &str) -> ! {
    cpu::interrupts_off();
    crate::smp::stop_others();
    klog!("freally-core: ERROR {what}. {detail}");
    crate::fault::write_last_panic(what, detail);
    #[cfg(feature = "ktest")]
    crate::test::panicked(what);
    if let Some(info) = crate::boot_info()
        && let Some(mut fb) = crate::fb::Fb::new(&info.framebuffer)
    {
        let mut line = Buf::<200>::new();
        let _ = core::fmt::write(&mut line, format_args!("{what}."));
        fscreen::panic(
            &mut fb,
            &[
                line.as_str(),
                detail,
                "Freally OS stopped so nothing else could go wrong. Restart the computer to carry on.",
            ],
        );
    }
    cpu::halt_forever()
}

// The probes: each touches one address; a fault lands on its fix-up, which
// returns 1 instead of 0. Listed in `.extable`.
core::arch::global_asm!(
    ".global probe_read",
    "probe_read:",
    "probe_read_insn:",
    "    mov al, [rdi]",
    "    xor eax, eax",
    "    ret",
    "probe_read_fix:",
    "    mov eax, 1",
    "    ret",
    ".global probe_write",
    "probe_write:",
    "probe_write_insn:",
    "    mov byte ptr [rdi], 0",
    "    xor eax, eax",
    "    ret",
    "probe_write_fix:",
    "    mov eax, 1",
    "    ret",
    ".global probe_exec",
    "probe_exec:",
    "    lea rax, [rip + probe_exec_back]",
    "    push rax",
    "probe_exec_insn:",
    "    jmp rdi",
    "probe_exec_back:",
    "    xor eax, eax",
    "    ret",
    ".global probe_exec_fix",
    "probe_exec_fix:",
    "    add rsp, 8",
    "    mov eax, 1",
    "    ret",
    ".section .extable, \"a\"",
    ".p2align 3",
    "    .quad probe_read_insn, probe_read_fix",
    "    .quad probe_write_insn, probe_write_fix",
    ".text",
);

unsafe extern "C" {
    fn probe_read(addr: u64) -> u32;
    fn probe_write(addr: u64) -> u32;
    fn probe_exec(addr: u64) -> u32;
    fn probe_exec_fix();
}

/// `Ok` if the kernel can run code at `addr` (it should hold a `ret`), else
/// the fault's vector. Used to prove SMEP and NX.
pub fn try_exec(addr: u64) -> Result<(), u64> {
    EXEC_PROBE.store(true, Ordering::SeqCst);
    // SAFETY: a fault on the jump target resumes at probe_exec_fix.
    let r = unsafe { probe_exec(addr) };
    EXEC_PROBE.store(false, Ordering::SeqCst);
    if r == 0 { Ok(()) } else { Err(LAST_FIXED_VECTOR.load(Ordering::Relaxed)) }
}

/// `Ok` if the kernel can read `addr`, else the fault's vector.
pub fn try_read(addr: u64) -> Result<(), u64> {
    // SAFETY: a fault here resumes at the fix-up.
    if unsafe { probe_read(addr) } == 0 { Ok(()) } else { Err(LAST_FIXED_VECTOR.load(Ordering::Relaxed)) }
}

/// `Ok` if the kernel can write a zero byte at `addr`, else the fault's vector.
///
/// # Safety
/// Writes a zero byte at `addr` if it's writable.
pub unsafe fn try_write(addr: u64) -> Result<(), u64> {
    // SAFETY: the caller allows the write; a fault resumes at the fix-up.
    if unsafe { probe_write(addr) } == 0 { Ok(()) } else { Err(LAST_FIXED_VECTOR.load(Ordering::Relaxed)) }
}

/// Registers a handler for a vector in the IDT's IRQ table.
pub fn route(vector: u8, h: idt::IrqHandler) {
    idt::set_handler(vector, h);
}
