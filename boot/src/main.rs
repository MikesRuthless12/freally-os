// SPDX-License-Identifier: Apache-2.0
//! freally-boot: Freally OS's own UEFI bootloader. Placeholder until TASK-007.
#![no_std]
#![no_main]

#[unsafe(no_mangle)]
extern "efiapi" fn efi_main(_image: *mut core::ffi::c_void, _st: *mut core::ffi::c_void) -> usize {
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
