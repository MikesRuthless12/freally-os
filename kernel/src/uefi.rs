// SPDX-License-Identifier: Apache-2.0
//! UEFI runtime services (TASK-045): `SetVariable`, for `FreallyBootCtl` and
//! `FreallyClock` and nothing else (syscall 35, conductor only);
//! `GetVariable`, for `FreallyClock` at boot (TASK-180A); `ResetSystem`, to
//! power off (TASK-102: the login screen's Shut down, through conductor).
//!
//! The bootloader moved the runtime services into the direct map
//! (`SetVirtualAddressMap`) and handed them over only when the firmware has
//! a Memory Attributes Table, so their code is mapped read-only and
//! executable and their data no-execute (`paging::init`): W^X holds here
//! too. A call runs with interrupts off and the caller's FPU state saved
//! (firmware may use SSE).

use crate::sync::SpinLock;
use alloc::vec::Vec;
use fcore_abi::{FErr, FREALLY_VENDOR_GUID, Guid};

/// The runtime services table, as far as `ResetSystem`.
#[repr(C)]
struct RuntimeServices {
    _header: [u8; 24],
    _time: [usize; 4],
    _set_virtual_address_map: usize,
    _convert_pointer: usize,
    get_variable: extern "efiapi" fn(*const u16, *const Guid, *mut u32, *mut usize, *mut u8) -> usize,
    _get_next_variable_name: usize,
    set_variable: extern "efiapi" fn(*const u16, *const Guid, u32, usize, *const u8) -> usize,
    _get_next_high_monotonic_count: usize,
    reset_system: extern "efiapi" fn(u32, usize, usize, *const u8),
}

/// `EfiResetShutdown`.
const RESET_SHUTDOWN: u32 = 2;

/// Powers the machine off through the firmware. Returns only if it can't.
pub fn power_off() -> FErr {
    let rt = crate::boot_info().map_or(0, |b| b.uefi_runtime);
    if rt == 0 {
        return FErr::NotSupported;
    }
    let _one = CALL.lock();
    crate::arch::cpu::interrupts_off();
    crate::sched::with_fpu_saved(|| {
        // SAFETY: as `with_runtime`'s; ResetSystem takes no memory of ours.
        unsafe { ((*(rt as *const RuntimeServices)).reset_system)(RESET_SHUTDOWN, 0, 0, core::ptr::null()) }
    });
    FErr::Internal
}

/// One runtime call at a time: firmware isn't reentrant.
static CALL: SpinLock<()> = SpinLock::new(());

/// `call` with the firmware's runtime table: one call at a time, interrupts
/// off and the FPU's state saved around it; `None` when the firmware gave
/// none (no Memory Attributes Table, or none at all).
fn with_runtime<R>(call: impl FnOnce(&RuntimeServices) -> R) -> Option<R> {
    let rt = crate::boot_info().map_or(0, |b| b.uefi_runtime);
    if rt == 0 {
        return None;
    }
    let _one = CALL.lock();
    let irq = crate::arch::cpu::interrupts_enabled();
    crate::arch::cpu::interrupts_off();
    // SAFETY: the firmware's runtime table at its direct-map address
    // (SetVirtualAddressMap), its code mapped executable.
    let r = crate::sched::with_fpu_saved(|| call(unsafe { &*(rt as *const RuntimeServices) }));
    if irq {
        crate::arch::cpu::interrupts_on();
    }
    Some(r)
}

/// `FreallyClock` as the firmware keeps it, read once at boot; `None` when
/// it has none, it's damaged, or there are no runtime services.
pub fn get_clock() -> Option<fbootctl::clock::Zone> {
    let name: Vec<u16> = fbootctl::clock::NAME.encode_utf16().chain(core::iter::once(0)).collect();
    let mut data = [0u8; fbootctl::clock::SIZE];
    let mut len = data.len();
    // `len` says how much `data` holds; a variable longer than that is
    // refused (EFI_BUFFER_TOO_SMALL). The name and data are kernel memory,
    // alive for the call.
    let status = with_runtime(|rt| {
        (rt.get_variable)(name.as_ptr(), &FREALLY_VENDOR_GUID, core::ptr::null_mut(), &mut len, data.as_mut_ptr())
    })?;
    if status != 0 {
        return None;
    }
    fbootctl::clock::Zone::decode(data.get(..len)?)
}

/// Writes `name` (one of ours: non-volatile, boot and runtime access).
pub fn set_var(name: &str, data: &[u8]) -> Result<(), FErr> {
    let name: Vec<u16> = name.encode_utf16().chain(core::iter::once(0)).collect();
    // The name and data are kernel memory, alive for the call.
    let status = with_runtime(|rt| {
        (rt.set_variable)(name.as_ptr(), &FREALLY_VENDOR_GUID, fbootctl::ATTRIBUTES, data.len(), data.as_ptr())
    })
    .ok_or(FErr::NotSupported)?;
    match status & !(1 << 63) {
        0 => Ok(()),
        2 => Err(FErr::InvalidArgs),   // EFI_INVALID_PARAMETER
        8 => Err(FErr::AccessDenied),  // EFI_WRITE_PROTECTED
        9 => Err(FErr::NoMemory),      // EFI_OUT_OF_RESOURCES (NVRAM full)
        15 => Err(FErr::AccessDenied), // EFI_SECURITY_VIOLATION
        _ => Err(FErr::Internal),
    }
}
