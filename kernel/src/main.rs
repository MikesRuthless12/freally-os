// SPDX-License-Identifier: Apache-2.0
//! Freally Core, the microkernel.
//!
//! The bootloader verified the image this came from, mapped it W^X at
//! `KERNEL_BASE`, mapped all physical memory at `DIRECT_MAP`, and jumped here
//! with `BootInfo` in RDI (TASK-018). From here: the CPU's tables (TASK-019),
//! physical memory and the kernel's own page tables (TASK-020), the heap
//! (TASK-021), and with the `ktest` feature, every test (TASK-022).
#![no_std]
#![no_main]
// Phase 1 lands piece by piece: primitives arrive a task before their callers.
// This allowance goes at the combined closeout's /simplify (HANDOFF.md).
#![allow(dead_code)]

#[cfg(not(any(feature = "flavour-release", feature = "flavour-dev", feature = "flavour-e2e")))]
compile_error!("build the kernel through `cargo xtask build --flavour release|dev|e2e`");

extern crate alloc;

#[macro_use]
mod serial;
#[macro_use]
mod test;
mod acpi_early;
mod arch;
mod exceptions;
mod fault;
mod fb;
mod fmtbuf;
mod futex;
mod hw;
mod init;
mod ipc;
mod mm;
mod obj;
mod proc;
mod rand;
mod sched;
mod smp;
mod sync;
mod syscall;
#[cfg(feature = "ktest")]
mod syscall_tests;
mod time;
mod timeline;
mod timer;
mod uefi;

use arch::cpu;
use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use fcore_abi::{BOOTINFO_MAGIC, BOOTINFO_VERSION, BootInfo, MemKind};

static BOOT_INFO: AtomicPtr<BootInfo> = AtomicPtr::new(core::ptr::null_mut());

/// The boot timeline's kernel stamps (raw TSC): K0 entry, K1 memory ready.
pub static K0: AtomicU64 = AtomicU64::new(0);
pub static K1: AtomicU64 = AtomicU64::new(0);
pub static K2: AtomicU64 = AtomicU64::new(0);

static ACPI: AtomicPtr<acpi_early::Tables> = AtomicPtr::new(core::ptr::null_mut());

/// The ACPI tables the kernel parsed at boot.
pub fn acpi() -> Option<&'static acpi_early::Tables> {
    let p = ACPI.load(Ordering::Acquire);
    // SAFETY: set once to a leaked allocation that lives forever.
    if p.is_null() { None } else { Some(unsafe { &*p }) }
}

/// The scheduler's hook on every timer interrupt.
pub fn sched_tick() {
    sched::tick();
}

/// BootInfo, once the kernel has checked it.
pub fn boot_info() -> Option<&'static BootInfo> {
    let p = BOOT_INFO.load(Ordering::Acquire);
    // SAFETY: set once from the bootloader's BootInfo, which lives in BOOT_DATA
    // memory the kernel never frees.
    if p.is_null() { None } else { Some(unsafe { &*p }) }
}

/// The kernel's first instruction.
///
/// # Safety
/// Only the bootloader calls this, once, with a valid `BootInfo`.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "sysv64" fn _start(info: *const BootInfo) -> ! {
    cpu::interrupts_off(); // until the IDT and the APIC are the kernel's own
    K0.store(cpu::rdtsc(), Ordering::Relaxed);
    // SAFETY: the bootloader passes BootInfo's direct-map address.
    let bi = unsafe { &*info };
    klog!(
        "freally-core {} entered (K0 {} ticks after B0)",
        fcore_abi::VERSION,
        K0.load(Ordering::Relaxed).saturating_sub(bi.tsc.b0)
    );
    if bi.magic != BOOTINFO_MAGIC || bi.version != BOOTINFO_VERSION {
        klog!("freally-core: ERROR BootInfo has the wrong magic or version");
        cpu::halt_forever();
    }
    BOOT_INFO.store(info as *mut BootInfo, Ordering::Release);

    // TASK-019: the CPU's own tables, so every fault has a handler; TASK-036:
    // its block at GS.
    smp::init_bsp();
    arch::idt::init();

    // TASK-020: physical memory and the kernel's own page tables.
    // SAFETY: the map lives in the bootloader's BOOT_DATA pages, in the direct map.
    let map = unsafe { core::slice::from_raw_parts(bi.memory_map.ptr, bi.memory_map.len as usize) };
    if let Err(e) = mm::frames::init(map) {
        exceptions::stop(e, "The memory map has no room for Freally Core.");
    }
    // TASK-031: before anything else allocates a frame.
    // TASK-031: before anything else allocates a frame.
    fault::claim_last_panic_page();
    if let Err(e) = mm::paging::init(bi, map) {
        exceptions::stop(e, "Freally Core couldn't build its page tables.");
    }
    K1.store(cpu::rdtsc(), Ordering::Relaxed);
    let usable: u64 = map.iter().filter(|r| r.kind == MemKind::Usable).map(|r| r.len).sum();
    klog!(
        "freally-core: memory ready: {} MiB usable, {} MiB free, own page tables (K1 {} ticks after K0)",
        usable >> 20,
        (mm::frames::free_count() * 4096) >> 20,
        K1.load(Ordering::Relaxed) - K0.load(Ordering::Relaxed)
    );

    // TASK-023: interrupts and time.
    let tables = match acpi_early::parse(bi.acpi_rsdp) {
        Ok(t) => t,
        Err(e) => exceptions::stop(e, "The firmware's ACPI tables are missing or damaged."),
    };
    let apic_id = arch::apic::init(tables.madt.lapic_addr, tables.madt.has_8259);
    arch::ioapic::init(&tables.madt);
    exceptions::route(arch::apic::TIMER_VECTOR, timer::interrupt);
    exceptions::route(arch::apic::SPURIOUS_VECTOR, timer::spurious);
    exceptions::route(arch::apic::RESCHED_VECTOR, sched::resched_interrupt);
    let source = time::init(tables.hpet, bi.tsc.b0);
    K2.store(cpu::rdtsc(), Ordering::Relaxed);
    klog!(
        "freally-core: interrupts and time ready: {} (APIC {}), {} CPUs, {} I/O APICs, TSC {} MHz from {}{}, RTC {} (K2 {} µs after K1)",
        if arch::apic::x2apic() { "x2APIC" } else { "xAPIC" },
        apic_id,
        tables.madt.cpu_count,
        tables.madt.ioapic_count,
        time::tsc_hz() / 1_000_000,
        source.how,
        match source.check {
            Some(h) => alloc::format!(" (HPET says {} MHz)", h / 1_000_000),
            None => alloc::string::String::new(),
        },
        time::rtc_unix(),
        time::ticks_to_ns(K2.load(Ordering::Relaxed) - K1.load(Ordering::Relaxed)) / 1000
    );
    ACPI.store(alloc::boxed::Box::leak(alloc::boxed::Box::new(tables)), Ordering::Release);
    for (id, t) in [
        (timeline::B0, bi.tsc.b0),
        (timeline::B1, bi.tsc.b1),
        (timeline::B2, bi.tsc.b2),
        (timeline::B3, bi.tsc.b3),
        (timeline::K0, K0.load(Ordering::Relaxed)),
        (timeline::K1, K1.load(Ordering::Relaxed)),
        (timeline::K2, K2.load(Ordering::Relaxed)),
    ] {
        timeline::stamp(id, t);
    }

    // TASK-024–028: protections, system calls, the scheduler.
    let (smep, smap, pcid) = mm::aspace::init_cpu_protections();
    syscall::init_cpu();
    if let Err(e) = sched::init() {
        exceptions::stop(e, "Freally Core couldn't start its scheduler.");
    }
    klog!("freally-core: scheduler ready (SMEP {smep}, SMAP {smap}, PCID {pcid})");

    // TASK-036: every other CPU, K3.
    smp::start_aps();

    #[cfg(feature = "bench-ipc")]
    {
        let median = ipc::bench();
        klog!("bench ipc: median {} ns per round trip (10,000 calls)", median);
        cpu::qemu_exit(0x10);
    }

    #[cfg(feature = "ktest")]
    test::run_all();
    #[cfg(not(feature = "ktest"))]
    running(bi)
}

/// The end of initialisation, until the scheduler exists: the title, then idle.
#[cfg(not(feature = "ktest"))]
fn running(bi: &BootInfo) -> ! {
    #[cfg(feature = "crash-ud2")]
    // SAFETY: deliberately invalid, to prove the panic screen (a test build only).
    unsafe {
        core::arch::asm!("ud2")
    };

    if let Some(mut f) = fb::Fb::new(&bi.framebuffer) {
        fscreen::title(&mut f, fcore_abi::VERSION, "Freally Core is running.");
    }
    klog!("freally-core: showing the title");

    // TASK-030: conductor, PID 1. The boot thread's work is done.
    if let Err(e) = init::start_conductor() {
        exceptions::stop(e, "Freally Core couldn't start conductor.");
    }
    sched::exit_current()
}

#[panic_handler]
fn panic(p: &core::panic::PanicInfo) -> ! {
    let mut what = fmtbuf::Buf::<200>::new();
    let _ = core::fmt::write(&mut what, format_args!("Freally Core panicked: {}", p.message()));
    let mut at = fmtbuf::Buf::<160>::new();
    if let Some(l) = p.location() {
        let _ = core::fmt::write(&mut at, format_args!("At {}:{}.", l.file(), l.line()));
    }
    exceptions::stop(what.as_str(), at.as_str())
}
