// SPDX-License-Identifier: Apache-2.0
//! Copies between the kernel and user memory (TASK-025). Every user pointer is
//! range-checked against the user half first; the copy itself is a `rep movsb`
//! inside a STAC/CLAC window (with SMAP) listed in the exception table, so an
//! unmapped or forbidden page makes the copy fail, never the kernel.

use super::aspace::USER_TOP;
use core::sync::atomic::{AtomicBool, Ordering};
use fcore_abi::FErr;

static SMAP: AtomicBool = AtomicBool::new(false);

pub fn set_smap(on: bool) {
    SMAP.store(on, Ordering::Relaxed);
}

core::arch::global_asm!(
    // user_copy_smap(dst, src, len) -> 0 ok, 1 fault
    ".global user_copy_smap",
    "user_copy_smap:",
    "    mov rcx, rdx",
    "    stac",
    "user_copy_smap_insn:",
    "    rep movsb",
    "    clac",
    "    xor eax, eax",
    "    ret",
    "user_copy_smap_fix:",
    "    clac",
    "    mov eax, 1",
    "    ret",
    // The same without STAC/CLAC, for CPUs without SMAP (STAC would fault there).
    ".global user_copy_plain",
    "user_copy_plain:",
    "    mov rcx, rdx",
    "user_copy_plain_insn:",
    "    rep movsb",
    "    xor eax, eax",
    "    ret",
    "user_copy_plain_fix:",
    "    mov eax, 1",
    "    ret",
    ".section .extable, \"a\"",
    ".p2align 3",
    "    .quad user_copy_smap_insn, user_copy_smap_fix",
    "    .quad user_copy_plain_insn, user_copy_plain_fix",
    ".text",
);

unsafe extern "C" {
    fn user_copy_smap(dst: *mut u8, src: *const u8, len: usize) -> u32;
    fn user_copy_plain(dst: *mut u8, src: *const u8, len: usize) -> u32;
}

fn user_range(addr: u64, len: usize) -> Result<(), FErr> {
    if addr.checked_add(len as u64).is_none_or(|e| e > USER_TOP) || addr < 0x1000 {
        return Err(FErr::InvalidArgs);
    }
    Ok(())
}

fn copy(dst: *mut u8, src: *const u8, len: usize) -> Result<(), FErr> {
    // SAFETY: the caller checked the user side's range; a fault resumes at the fix-up.
    let r = unsafe {
        if SMAP.load(Ordering::Relaxed) { user_copy_smap(dst, src, len) } else { user_copy_plain(dst, src, len) }
    };
    if r == 0 { Ok(()) } else { Err(FErr::InvalidArgs) }
}

/// Copies `dst.len()` bytes from user address `src`.
pub fn copy_from_user(dst: &mut [u8], src: u64) -> Result<(), FErr> {
    user_range(src, dst.len())?;
    copy(dst.as_mut_ptr(), src as *const u8, dst.len())
}

/// Copies `src` to user address `dst`.
pub fn copy_to_user(dst: u64, src: &[u8]) -> Result<(), FErr> {
    user_range(dst, src.len())?;
    copy(dst as *mut u8, src.as_ptr(), src.len())
}

/// Reads one `T` from user memory.
pub fn read_user<T: Copy + Default>(src: u64) -> Result<T, FErr> {
    let mut v = T::default();
    // SAFETY: `v` is a plain value of size_of::<T>() bytes.
    let bytes = unsafe { core::slice::from_raw_parts_mut(&mut v as *mut T as *mut u8, core::mem::size_of::<T>()) };
    copy_from_user(bytes, src)?;
    Ok(v)
}

/// Writes one `T` to user memory.
pub fn write_user<T: Copy>(dst: u64, v: &T) -> Result<(), FErr> {
    // SAFETY: as above.
    let bytes = unsafe { core::slice::from_raw_parts(v as *const T as *const u8, core::mem::size_of::<T>()) };
    copy_to_user(dst, bytes)
}
