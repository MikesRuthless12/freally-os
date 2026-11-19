// SPDX-License-Identifier: Apache-2.0
//! Device memory, read and written with plain `mov`s (TASK-092A).
//!
//! In a virtual machine every access to a device's registers is trapped and
//! carried out by the hypervisor's instruction emulator, and KVM's refuses
//! vector instructions: an NVMe register written with an AVX store (`vmovss`,
//! the compiler's choice for a value it held in a vector register) came back
//! to the driver as an invalid instruction. `read_volatile` and
//! `write_volatile` promise one access of the right size, not which
//! instruction makes it; these name the instruction. Drivers use them for
//! every register, configuration-space and MSI-X table access.

use core::arch::asm;

/// # Safety
/// `at` is mapped device memory (or memory) this process may read.
pub unsafe fn read8(at: u64) -> u8 {
    let v: u8;
    // SAFETY: as this function's.
    unsafe {
        asm!("mov {v}, byte ptr [{at}]", at = in(reg) at, v = out(reg_byte) v, options(nostack, preserves_flags))
    };
    v
}

/// # Safety
/// As [`read8`]'s; `at` is 2-byte aligned.
pub unsafe fn read16(at: u64) -> u16 {
    let v: u16;
    // SAFETY: as this function's.
    unsafe { asm!("mov {v:x}, word ptr [{at}]", at = in(reg) at, v = out(reg) v, options(nostack, preserves_flags)) };
    v
}

/// # Safety
/// As [`read8`]'s; `at` is 4-byte aligned.
pub unsafe fn read32(at: u64) -> u32 {
    let v: u32;
    // SAFETY: as this function's.
    unsafe { asm!("mov {v:e}, dword ptr [{at}]", at = in(reg) at, v = out(reg) v, options(nostack, preserves_flags)) };
    v
}

/// # Safety
/// As [`read8`]'s; `at` is 8-byte aligned.
pub unsafe fn read64(at: u64) -> u64 {
    let v: u64;
    // SAFETY: as this function's.
    unsafe { asm!("mov {v}, qword ptr [{at}]", at = in(reg) at, v = out(reg) v, options(nostack, preserves_flags)) };
    v
}

/// # Safety
/// `at` is mapped device memory (or memory) this process may write; 2-byte aligned.
pub unsafe fn write16(at: u64, v: u16) {
    // SAFETY: as this function's.
    unsafe { asm!("mov word ptr [{at}], {v:x}", at = in(reg) at, v = in(reg) v, options(nostack, preserves_flags)) };
}

/// # Safety
/// As [`write16`]'s; 4-byte aligned.
pub unsafe fn write32(at: u64, v: u32) {
    // SAFETY: as this function's.
    unsafe { asm!("mov dword ptr [{at}], {v:e}", at = in(reg) at, v = in(reg) v, options(nostack, preserves_flags)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_width_reads_and_writes_its_own_bytes() {
        let mut m = [0u8; 16];
        let at = m.as_mut_ptr() as u64;
        // SAFETY: inside `m`, aligned.
        unsafe {
            write32(at + 4, 0xDDCC_BBAA);
            write16(at + 8, 0x2211);
            assert_eq!(read8(at + 5), 0xBB);
            assert_eq!(read16(at + 4), 0xBBAA);
            assert_eq!(read32(at + 4), 0xDDCC_BBAA);
            assert_eq!(read64(at + 4), 0x0000_2211_DDCC_BBAA);
        }
        assert_eq!(m[3], 0, "nothing written before");
        assert_eq!(&m[4..10], &[0xAA, 0xBB, 0xCC, 0xDD, 0x11, 0x22]);
        assert_eq!(m[10], 0, "nothing written after");
    }
}
