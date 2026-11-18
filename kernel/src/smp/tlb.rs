// SPDX-License-Identifier: Apache-2.0
//! TLB shootdowns (TASK-036). A CPU that changes or removes a page's entry
//! drops it from its own TLB (`invlpg`, in `paging`); every other CPU that may
//! hold it is asked to do the same, and the change isn't done until they all
//! have: only then may the page's frame be freed or reused.
//!
//! - **Who:** a kernel address (shared by every address space, global
//!   pages): every other online CPU. A user address: the CPUs with that
//!   address space loaded now. One that loads it later flushes it as it does
//!   (every CR3 load clears the new PCID's entries), and each CPU records its
//!   PML4 before loading it, so a CPU is either asked or loads afterwards.
//! - **How:** each asking CPU has its own request; a target gets an NMI and a
//!   bit in its pending mask, does every request pending, and counts each one
//!   down. NMIs arrive even on a CPU spinning on a lock with interrupts off,
//!   so a shootdown made while holding one can't deadlock: two CPUs asking at
//!   once each serve the other's NMI while they wait.

use super::{Cpu, MAX_CPUS, count, cpu, this};
use crate::arch::{apic, cpu as x86};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};

/// Past this many pages, a target flushes everything instead.
const FULL_FLUSH_PAGES: u64 = 64;

/// One asking CPU's request: the PML4 (0 for kernel addresses), the first
/// page and how many, and how many CPUs still have to do it.
struct Request {
    space: AtomicU64,
    start: AtomicU64,
    pages: AtomicU64,
    left: AtomicU32,
}

static REQUESTS: [Request; MAX_CPUS] = [const {
    Request { space: AtomicU64::new(0), start: AtomicU64::new(0), pages: AtomicU64::new(0), left: AtomicU32::new(0) }
}; MAX_CPUS];

/// Shootdowns asked for (all CPUs); for the tests and Monitor.
pub static ASKED: AtomicU64 = AtomicU64::new(0);

const ADDR: u64 = 0x000F_FFFF_FFFF_F000;

/// Kernel addresses changed: every other CPU drops `pages` pages from `start`.
pub fn kernel(start: u64, pages: u64) {
    shoot(0, start, pages);
}

/// User addresses of the space with PML4 `pml4` changed.
pub fn user(pml4: u64, start: u64, pages: u64) {
    shoot(pml4, start, pages);
}

fn shoot(space: u64, start: u64, pages: u64) {
    if super::online() <= 1 || pages == 0 {
        return;
    }
    let _irq = x86::IrqOff::new();
    let me = this();
    let i = me.index as usize;
    let r = &REQUESTS[i];
    r.space.store(space, Ordering::Relaxed);
    r.start.store(start, Ordering::Relaxed);
    r.pages.store(pages, Ordering::Relaxed);
    // The page tables were written before this; which CPUs have the space
    // loaded is read after it (each stores its PML4 before loading it).
    fence(Ordering::SeqCst);
    let mut targets = 0u64;
    for j in (0..count()).filter(|&j| j != i) {
        let Some(c) = cpu(j) else { continue };
        if c.online.load(Ordering::Acquire) && (space == 0 || c.pml4.load(Ordering::SeqCst) == space) {
            targets |= 1 << j;
        }
    }
    if targets == 0 {
        return;
    }
    r.left.store(targets.count_ones(), Ordering::SeqCst);
    ASKED.fetch_add(1, Ordering::Relaxed);
    for j in 0..MAX_CPUS {
        if targets & (1 << j) != 0
            && let Some(c) = cpu(j)
        {
            c.tlb_pending.fetch_or(1 << i, Ordering::SeqCst);
            apic::send_nmi(c.apic_id.load(Ordering::Acquire));
        }
    }
    while r.left.load(Ordering::Acquire) != 0 {
        core::hint::spin_loop();
    }
}

/// The NMI's work on CPU `c`: every shootdown pending, until none is.
pub fn serve(c: &Cpu) {
    loop {
        let pending = c.tlb_pending.swap(0, Ordering::AcqRel);
        if pending == 0 {
            return;
        }
        for i in (0..MAX_CPUS).filter(|i| pending & (1 << i) != 0) {
            let r = &REQUESTS[i];
            drop_entries(
                r.space.load(Ordering::Relaxed),
                r.start.load(Ordering::Relaxed),
                r.pages.load(Ordering::Relaxed),
            );
            r.left.fetch_sub(1, Ordering::Release);
        }
    }
}

fn drop_entries(space: u64, start: u64, pages: u64) {
    // A user space not loaded here any more: its next load flushes it.
    if space != 0 && x86::cr3() & ADDR != space {
        return;
    }
    if pages > FULL_FLUSH_PAGES {
        if space == 0 {
            // Global pages too: toggling CR4.PGE drops every entry.
            let c4 = x86::cr4();
            // SAFETY: PGE off and on again; nothing else changes.
            unsafe {
                x86::set_cr4(c4 & !(1 << 7));
                x86::set_cr4(c4);
            }
        } else {
            // SAFETY: the same CR3 again (bit 63 clear): this PCID's entries go.
            unsafe { x86::set_cr3(x86::cr3()) };
        }
        return;
    }
    for p in 0..pages {
        // SAFETY: invlpg only drops a TLB entry.
        unsafe { core::arch::asm!("invlpg [{}]", in(reg) start + p * 4096, options(nostack, preserves_flags)) };
    }
}

crate::ktest!(tlb_every_cpu_sees_a_remapped_kernel_page_at_once, {
    use crate::mm::{frames, paging};
    use crate::sched::{self, Class};
    use core::sync::atomic::AtomicBool;
    use fcore_abi::layout::DIRECT_MAP;
    // The page under test: the lowest of a fresh kernel stack's pages.
    static VA: AtomicU64 = AtomicU64::new(0);
    static GEN: AtomicU64 = AtomicU64::new(0);
    static STOP: AtomicBool = AtomicBool::new(false);
    static STALE: AtomicU64 = AtomicU64::new(0);
    static READS: AtomicU64 = AtomicU64::new(0);
    static RUNNING: AtomicU32 = AtomicU32::new(0);
    extern "C" fn reader(_: u64) {
        RUNNING.fetch_add(1, Ordering::SeqCst);
        let va = VA.load(Ordering::Acquire) as *const u64;
        while !STOP.load(Ordering::Acquire) {
            let g = GEN.load(Ordering::Acquire);
            // SAFETY: the page stays mapped (to one frame or another) until STOP.
            let v = unsafe { core::ptr::read_volatile(va) };
            // A value older than the generation read first: the old mapping, still cached.
            if v < g {
                STALE.fetch_add(1, Ordering::SeqCst);
            }
            READS.fetch_add(1, Ordering::Relaxed);
        }
        RUNNING.fetch_sub(1, Ordering::SeqCst);
    }
    let n = super::online();
    if n < 2 {
        klog!("    skipped: one CPU (this test needs -smp 2 or more)");
        return Ok(());
    }
    let top = sched::alloc_kernel_stack()?;
    let va = top - 64 * 1024;
    let original = paging::translate(paging::kernel_pml4(), va).ok_or("the test page isn't mapped")?.0;
    let frames: [u64; 4] = [0; 4].map(|_| frames::alloc_zeroed().unwrap_or(0));
    if frames.contains(&0) {
        return Err("out of frames");
    }
    VA.store(va, Ordering::Release);
    GEN.store(0, Ordering::SeqCst);
    STOP.store(false, Ordering::SeqCst);
    STALE.store(0, Ordering::SeqCst);
    let flags = paging::WRITABLE | paging::NX | paging::GLOBAL;
    // SAFETY: a fresh frame, through the direct map.
    unsafe { core::ptr::write_volatile((frames[0] + DIRECT_MAP) as *mut u64, 0) };
    paging::map(paging::kernel_pml4(), va, frames[0], flags)?;
    let mut readers = alloc::vec::Vec::new();
    for i in 0..n {
        let t = sched::create("tlb-reader", Class::Normal, reader, 0, false)?;
        sched::pin(&t, i);
        sched::start(&t);
        readers.push(t);
    }
    // This CPU's reader runs only when we sleep; wait for the others.
    while (RUNNING.load(Ordering::SeqCst) as usize) < n - 1 {
        sched::sleep_ns(1_000_000);
    }
    let rounds = 2000u64;
    for g in 1..=rounds {
        let f = frames[(g % 4) as usize];
        // SAFETY: the next frame, not mapped now, through the direct map.
        unsafe { core::ptr::write_volatile((f + DIRECT_MAP) as *mut u64, g) };
        paging::map(paging::kernel_pml4(), va, f, flags)?;
        kernel(va, 1);
        // Done: from here, no CPU may read the old frame through `va`.
        GEN.store(g, Ordering::SeqCst);
        // The old frame gets older values written into it, so a stale read shows.
        let old = frames[((g + 3) % 4) as usize];
        // SAFETY: as above.
        unsafe { core::ptr::write_volatile((old + DIRECT_MAP) as *mut u64, 0) };
    }
    STOP.store(true, Ordering::SeqCst);
    while readers.iter().any(|t| !t.is_dead()) {
        sched::sleep_ns(1_000_000);
    }
    paging::map(paging::kernel_pml4(), va, original, flags)?;
    kernel(va, 1);
    for f in frames {
        frames::free(f);
    }
    sched::free_kernel_stack(top);
    let stale = STALE.load(Ordering::SeqCst);
    klog!("    {} remaps across {} CPUs, {} reads, {} stale", rounds, n, READS.load(Ordering::Relaxed), stale);
    if stale != 0 { Err("a CPU read a page through a mapping that had been shot down") } else { Ok(()) }
});
