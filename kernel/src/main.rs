// SPDX-License-Identifier: Apache-2.0
//! Freally Core, the microkernel.
//!
//! The bootloader verified the image this came from, mapped it W^X at
//! `KERNEL_BASE`, mapped all physical memory at `DIRECT_MAP`, and jumped here
//! with `BootInfo` in RDI (TASK-018).
#![no_std]
#![no_main]

#[cfg(not(any(feature = "flavour-release", feature = "flavour-dev", feature = "flavour-e2e")))]
compile_error!("build the kernel through `cargo xtask build --flavour release|dev|e2e`");

#[macro_use]
mod serial;
mod fb;

use fcore_abi::{BOOTINFO_MAGIC, BOOTINFO_VERSION, BootInfo, MemKind};

fn rdtsc() -> u64 {
    // SAFETY: RDTSC has no side effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}

fn halt() -> ! {
    loop {
        // SAFETY: interrupts are off; this parks the CPU.
        unsafe { core::arch::asm!("cli; hlt", options(nomem, nostack)) };
    }
}

/// The kernel's first instruction.
///
/// # Safety
/// Only the bootloader calls this, once, with a valid `BootInfo`.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "sysv64" fn _start(info: *const BootInfo) -> ! {
    let k0 = rdtsc();
    // SAFETY: the bootloader passes BootInfo's direct-map address.
    let info = unsafe { &*info };
    klog!("freally-core {} entered (K0 {} ticks after B0)", fcore_abi::VERSION, k0.saturating_sub(info.tsc.b0));
    if info.magic != BOOTINFO_MAGIC || info.version != BOOTINFO_VERSION {
        klog!("freally-core: ERROR BootInfo has the wrong magic or version");
        halt();
    }
    // SAFETY: the map lives in the bootloader's BOOT_DATA pages, in the direct map.
    let map = unsafe { core::slice::from_raw_parts(info.memory_map.ptr, info.memory_map.len as usize) };
    let usable: u64 = map.iter().filter(|r| r.kind == MemKind::Usable).map(|r| r.len).sum();
    klog!(
        "freally-core: memory map {} regions, {} MiB usable; image {} bytes; RSDP {:#x}; runtime services {}",
        map.len(),
        usable >> 20,
        info.image.len,
        info.acpi_rsdp,
        if info.uefi_runtime != 0 { "mapped" } else { "absent" }
    );
    if let Some(mut f) = fb::Fb::new(&info.framebuffer) {
        fscreen::title(&mut f, fcore_abi::VERSION, "Freally Core is running.");
    }
    klog!("freally-core: showing the title");
    halt();
}

#[panic_handler]
fn panic(p: &core::panic::PanicInfo) -> ! {
    klog!("freally-core: ERROR panic: {p}");
    halt();
}
