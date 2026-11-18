// SPDX-License-Identifier: Apache-2.0
//! Small CPU primitives: ports, MSRs, control registers, interrupts.

#[inline]
pub fn outb(port: u16, v: u8) {
    // SAFETY: port I/O has no memory effects the compiler must know about.
    unsafe { core::arch::asm!("out dx, al", in("dx") port, in("al") v, options(nomem, nostack, preserves_flags)) };
}

#[inline]
pub fn inb(port: u16) -> u8 {
    let v: u8;
    // SAFETY: as above.
    unsafe { core::arch::asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags)) };
    v
}

#[inline]
pub fn outl(port: u16, v: u32) {
    // SAFETY: as above.
    unsafe { core::arch::asm!("out dx, eax", in("dx") port, in("eax") v, options(nomem, nostack, preserves_flags)) };
}

/// # Safety
/// Reading an MSR the CPU doesn't have faults.
#[inline]
pub unsafe fn rdmsr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: the caller names an MSR this CPU has.
    unsafe {
        core::arch::asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags))
    };
    ((hi as u64) << 32) | lo as u64
}

/// # Safety
/// Writing an MSR changes how the CPU behaves.
#[inline]
pub unsafe fn wrmsr(msr: u32, v: u64) {
    // SAFETY: the caller knows what the MSR does.
    unsafe {
        core::arch::asm!("wrmsr", in("ecx") msr, in("eax") v as u32, in("edx") (v >> 32) as u32, options(nostack, preserves_flags))
    };
}

#[inline]
pub fn cr2() -> u64 {
    let v: u64;
    // SAFETY: reading CR2 has no side effects.
    unsafe { core::arch::asm!("mov {}, cr2", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

#[inline]
pub fn cr3() -> u64 {
    let v: u64;
    // SAFETY: as above.
    unsafe { core::arch::asm!("mov {}, cr3", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

/// # Safety
/// `pml4` must map everything the kernel is using right now.
#[inline]
pub unsafe fn set_cr3(pml4: u64) {
    // SAFETY: the caller guarantees the tables are complete.
    unsafe { core::arch::asm!("mov cr3, {}", in(reg) pml4, options(nostack, preserves_flags)) };
}

#[inline]
pub fn cr0() -> u64 {
    let v: u64;
    // SAFETY: reading has no side effects.
    unsafe { core::arch::asm!("mov {}, cr0", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

/// # Safety
/// Changes paging and protection behaviour.
#[inline]
pub unsafe fn set_cr0(v: u64) {
    // SAFETY: the caller knows the bits it sets.
    unsafe { core::arch::asm!("mov cr0, {}", in(reg) v, options(nostack, preserves_flags)) };
}

#[inline]
pub fn cr4() -> u64 {
    let v: u64;
    // SAFETY: reading has no side effects.
    unsafe { core::arch::asm!("mov {}, cr4", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

/// # Safety
/// Changes paging and protection behaviour.
#[inline]
pub unsafe fn set_cr4(v: u64) {
    // SAFETY: the caller knows the bits it sets.
    unsafe { core::arch::asm!("mov cr4, {}", in(reg) v, options(nostack, preserves_flags)) };
}

#[inline]
pub fn rdtsc() -> u64 {
    // SAFETY: RDTSC has no side effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}

#[inline]
pub fn cpuid(leaf: u32, sub: u32) -> core::arch::x86_64::CpuidResult {
    core::arch::x86_64::__cpuid_count(leaf, sub)
}

#[inline]
pub fn interrupts_off() {
    // SAFETY: masking interrupts is always allowed in ring 0.
    unsafe { core::arch::asm!("cli", options(nomem, nostack)) };
}

/// Whether interrupts are on (RFLAGS.IF).
#[inline]
pub fn interrupts_enabled() -> bool {
    let f: u64;
    // SAFETY: reads RFLAGS.
    unsafe { core::arch::asm!("pushfq; pop {}", out(reg) f, options(nomem, preserves_flags)) };
    f & (1 << 9) != 0
}

#[inline]
pub fn interrupts_on() {
    // SAFETY: the IDT is loaded before anything turns interrupts on.
    unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
}

/// Interrupts off until dropped, then back as they were.
pub struct IrqOff(bool);

impl IrqOff {
    pub fn new() -> IrqOff {
        let was = interrupts_enabled();
        interrupts_off();
        IrqOff(was)
    }
}

impl Drop for IrqOff {
    fn drop(&mut self) {
        if self.0 {
            interrupts_on();
        }
    }
}

pub fn halt_forever() -> ! {
    loop {
        interrupts_off();
        // SAFETY: parks the CPU.
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
}

/// QEMU's `isa-debug-exit` (port 0xF4): QEMU exits with `(code << 1) | 1`.
pub fn qemu_exit(code: u8) -> ! {
    outl(0xF4, code as u32);
    halt_forever()
}
