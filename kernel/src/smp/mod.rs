// SPDX-License-Identifier: Apache-2.0
//! Every CPU (TASK-036): each one's block at GS, starting the other CPUs (the
//! APs), and the IPIs the scheduler and the panic screen send.
//!
//! - **The block:** each CPU's own data, reached through GS in the kernel. Its
//!   first words are the syscall entry's: `gs:[0]` the running thread's
//!   kernel stack, `gs:[8]` the user RSP while switching, then `gs:[16]` the
//!   block's own address. Read it only with interrupts off: a thread moved to
//!   another CPU halfway through would read the wrong one.
//! - **Starting the APs:** every enabled CPU in the MADT gets an INIT, then
//!   (10 ms later) a SIPI, and a second 200 µs after that, as Intel's MP
//!   protocol asks. Each starts in real mode in the trampoline below 1 MiB
//!   (`trampoline.rs`), takes the next slot of its table (a stack and a
//!   block), and enters [`ap_main`] in long mode on the kernel's page tables.
//!   K3 is stamped when every CPU is online, or when the wait is over: a CPU
//!   that never starts is logged, not fatal.
//! - **NMIs** carry the TLB shootdowns (`tlb.rs`) and the panic stop: they
//!   arrive even on a CPU spinning on a lock with interrupts off. The NMI path
//!   finds its CPU by APIC ID, never through GS (an NMI can land on the
//!   instruction before a `swapgs`).

pub mod tlb;
mod trampoline;

use crate::arch::{apic, cpu, gdt, idt};
use crate::sync::SpinLock;
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};

/// The most CPUs Freally Core runs on.
pub const MAX_CPUS: usize = 64;
const NO_APIC: u32 = u32::MAX;

/// One CPU's own data.
#[repr(C)]
pub struct Cpu {
    /// `gs:[0]`: the running thread's kernel stack top (the syscall entry's).
    pub kernel_rsp: AtomicU64,
    /// `gs:[8]`: the user RSP while the syscall entry switches stacks.
    pub user_rsp: AtomicU64,
    /// `gs:[16]`: this block's own address.
    this: AtomicU64,
    /// Its place in the scheduler's list: 0 is the boot CPU.
    pub index: u32,
    pub apic_id: AtomicU32,
    /// Its core type (CPUID leaf 0x1A on a hybrid CPU: `fcore_abi::core_type`).
    pub core_type: AtomicU8,
    pub online: AtomicBool,
    /// Its GDT and TSS, and the tops of its #DF, NMI, #MC and entry stacks.
    pub tables: AtomicPtr<gdt::Tables>,
    stacks: [AtomicU64; 4],
    /// The thread running here (the scheduler's, TASK-026).
    pub current: AtomicPtr<crate::sched::Thread>,
    pub need_resched: AtomicBool,
    /// A `channel_call` is writing its request here (`sched::handoff`).
    pub handoff: AtomicBool,
    /// The TSC deadline this CPU's timer is armed for.
    pub armed: AtomicU64,
    /// The address space loaded here (its PML4), for shootdowns.
    pub pml4: AtomicU64,
    /// This CPU's timer: how often it has fired, and when it last did.
    pub fires: AtomicU64,
    pub last_fire: AtomicU64,
    /// The process whose I/O ports are open in this CPU's TSS, and those ports.
    pub io_owner: AtomicU32,
    pub io_open: SpinLock<Vec<(u16, u16)>>,
    /// Shootdowns asked of this CPU: a bit per asking CPU.
    pub tlb_pending: AtomicU64,
}

impl Cpu {
    const fn new(index: u32) -> Cpu {
        Cpu {
            kernel_rsp: AtomicU64::new(0),
            user_rsp: AtomicU64::new(0),
            this: AtomicU64::new(0),
            index,
            apic_id: AtomicU32::new(NO_APIC),
            core_type: AtomicU8::new(0),
            online: AtomicBool::new(false),
            tables: AtomicPtr::new(core::ptr::null_mut()),
            stacks: [const { AtomicU64::new(0) }; 4],
            current: AtomicPtr::new(core::ptr::null_mut()),
            need_resched: AtomicBool::new(false),
            handoff: AtomicBool::new(false),
            armed: AtomicU64::new(0),
            pml4: AtomicU64::new(0),
            fires: AtomicU64::new(0),
            last_fire: AtomicU64::new(0),
            io_owner: AtomicU32::new(0),
            io_open: SpinLock::new(Vec::new()),
            tlb_pending: AtomicU64::new(0),
        }
    }

    fn set_tables(&self, t: &gdt::CpuTables) {
        self.tables.store(t.tables, Ordering::Release);
        for (s, &top) in self.stacks.iter().zip(&t.stacks) {
            s.store(top, Ordering::Release);
        }
    }

    fn cpu_tables(&self) -> gdt::CpuTables {
        gdt::CpuTables {
            tables: self.tables.load(Ordering::Acquire),
            stacks: [0, 1, 2, 3].map(|i| self.stacks[i].load(Ordering::Acquire)),
        }
    }
}

static BSP: Cpu = Cpu::new(0);
/// Every CPU's block, by index; `COUNT` of them made.
static CPUS: [AtomicPtr<Cpu>; MAX_CPUS] = [const { AtomicPtr::new(core::ptr::null_mut()) }; MAX_CPUS];
static COUNT: AtomicUsize = AtomicUsize::new(0);
static ONLINE: AtomicUsize = AtomicUsize::new(0);
/// A panic stops every CPU: set once, by the first CPU to stop.
static STOPPING: AtomicBool = AtomicBool::new(false);

/// This CPU's block. Interrupts off (or a thread pinned to its CPU).
pub fn this() -> &'static Cpu {
    let p: u64;
    // SAFETY: GS holds this CPU's block from `init_bsp`/`ap_main` on, and
    // `gs:[16]` is the block's own address.
    unsafe { core::arch::asm!("mov {}, gs:[16]", out(reg) p, options(nostack, readonly, preserves_flags)) };
    // SAFETY: blocks live forever.
    unsafe { &*(p as *const Cpu) }
}

/// CPU `i`'s block, if it has one.
pub fn cpu(i: usize) -> Option<&'static Cpu> {
    let p = CPUS.get(i)?.load(Ordering::Acquire);
    // SAFETY: published blocks live forever.
    if p.is_null() { None } else { Some(unsafe { &*p }) }
}

/// The blocks made (the boot CPU and every AP sent a SIPI).
pub fn count() -> usize {
    COUNT.load(Ordering::Acquire)
}

/// Whether the CPU mixes core types (CPUID 7's hybrid bit, with leaf 0x1A).
pub fn hybrid() -> bool {
    cpu::cpuid(0, 0).eax >= 0x1A && cpu::cpuid(7, 0).edx & (1 << 15) != 0
}

/// The core type of the CPU this runs on (FR-009): leaf 0x1A's top byte on a
/// hybrid CPU, unknown otherwise.
fn own_core_type() -> u8 {
    if hybrid() { (cpu::cpuid(0x1A, 0).eax >> 24) as u8 } else { fcore_abi::core_type::UNKNOWN }
}

/// CPU `i`'s core type (unknown when it has no block).
pub fn core_type(i: usize) -> u8 {
    cpu(i).map_or(fcore_abi::core_type::UNKNOWN, |c| c.core_type.load(Ordering::Relaxed))
}

/// The CPUs running.
pub fn online() -> usize {
    ONLINE.load(Ordering::Acquire)
}

fn set_gs(c: &Cpu) {
    const GS_BASE: u32 = 0xC000_0101;
    const KERNEL_GS_BASE: u32 = 0xC000_0102;
    // SAFETY: GS is the kernel's per-CPU pointer; the user's GS base starts at 0
    // (swapped in by `swapgs` on the way to user mode).
    unsafe {
        cpu::wrmsr(GS_BASE, c as *const Cpu as u64);
        cpu::wrmsr(KERNEL_GS_BASE, 0);
    }
}

/// The boot CPU's GDT, TSS and block, before anything needs them.
pub fn init_bsp() {
    let c = &BSP;
    c.this.store(c as *const Cpu as u64, Ordering::Release);
    let t = gdt::bsp();
    c.set_tables(&t);
    // SAFETY: once, on the boot CPU, interrupts off.
    unsafe { gdt::load(&t) };
    // After the GDT: loading GS's selector cleared its base.
    set_gs(c);
    c.core_type.store(own_core_type(), Ordering::Release);
    c.online.store(true, Ordering::Release);
    CPUS[0].store(c as *const Cpu as *mut Cpu, Ordering::Release);
    COUNT.store(1, Ordering::Release);
    ONLINE.store(1, Ordering::Release);
}

fn wait_ns(ns: u64) {
    let until = cpu::rdtsc() + crate::time::ns_to_ticks(ns);
    while cpu::rdtsc() < until {
        core::hint::spin_loop();
    }
}

/// Starts every other CPU in the MADT and waits for them; stamps K3.
pub fn start_aps() {
    let me = apic::id();
    BSP.apic_id.store(me, Ordering::Release);
    let ids: Vec<u32> = crate::acpi()
        .map(|t| t.madt.cpus[..t.madt.cpu_count].iter().copied().filter(|&id| id != me).collect())
        .unwrap_or_default();
    let started = if ids.is_empty() { Ok(0) } else { start(&ids[..ids.len().min(MAX_CPUS - 1)]) };
    let k3 = cpu::rdtsc();
    crate::timeline::stamp(crate::timeline::K3, k3);
    let want = 1 + ids.len().min(MAX_CPUS - 1);
    match started {
        Ok(_) if online() == want => klog!(
            "freally-core: {} CPUs online (K3 {} µs after K2)",
            online(),
            crate::time::ticks_to_ns(k3.saturating_sub(crate::K2.load(Ordering::Relaxed))) / 1000
        ),
        Ok(_) => klog!("freally-core: WARNING only {} of {} CPUs started; carrying on with those", online(), want),
        Err(e) => klog!("freally-core: WARNING the other CPUs couldn't be started ({e}); carrying on with one"),
    }
}

fn start(ids: &[u32]) -> Result<usize, &'static str> {
    let tr = trampoline::Trampoline::prepare(ap_main)?;
    // Each AP's block, tables and boot stack (its idle thread's for good).
    for slot in 0..ids.len() {
        let c: &'static Cpu = Box::leak(Box::new(Cpu::new(slot as u32 + 1)));
        c.this.store(c as *const Cpu as u64, Ordering::Release);
        c.set_tables(&gdt::alloc()?);
        let stack = crate::sched::alloc_kernel_stack()?;
        tr.set_slot(slot, stack, c as *const Cpu as u64);
        CPUS[slot + 1].store(c as *const Cpu as *mut Cpu, Ordering::Release);
    }
    COUNT.store(1 + ids.len(), Ordering::Release);
    crate::sched::add_cpus(1 + ids.len());
    // INIT, 10 ms, SIPI, 200 µs, SIPI (one already running ignores the second).
    for &id in ids {
        apic::send_init(id);
    }
    wait_ns(10_000_000);
    for _ in 0..2 {
        for &id in ids {
            apic::send_sipi(id, tr.page());
        }
        wait_ns(200_000);
    }
    let until = cpu::rdtsc() + crate::time::ns_to_ticks(1_000_000_000);
    while online() < 1 + ids.len() && cpu::rdtsc() < until {
        core::hint::spin_loop();
    }
    Ok(online() - 1)
}

/// An AP's first Rust: on its boot stack, still on the trampoline's tables.
extern "C" fn ap_main(c: &'static Cpu) -> ! {
    // SAFETY: the kernel's tables map everything the kernel uses (the
    // trampoline's mapped only their upper half and the low 2 MiB).
    unsafe { cpu::set_cr3(crate::mm::paging::kernel_pml4()) };
    let t = c.cpu_tables();
    // SAFETY: once, on this CPU, interrupts off; the tables are this CPU's.
    unsafe { gdt::load(&t) };
    idt::load();
    set_gs(c);
    c.apic_id.store(apic::init_cpu(), Ordering::Release);
    c.core_type.store(own_core_type(), Ordering::Release);
    crate::syscall::init_cpu();
    crate::mm::aspace::init_cpu_protections();
    crate::sched::run_ap(c.index as usize)
}

/// Called by the scheduler once an AP's idle thread is in place.
pub fn mark_online(c: &Cpu) {
    c.online.store(true, Ordering::Release);
    ONLINE.fetch_add(1, Ordering::AcqRel);
}

/// Asks CPU `i` to reschedule.
pub fn resched(i: usize) {
    if let Some(c) = cpu(i) {
        apic::send_ipi(c.apic_id.load(Ordering::Acquire), apic::RESCHED_VECTOR);
    }
}

/// The CPU with APIC ID `id` (the NMI path: never through GS).
fn by_apic(id: u32) -> Option<&'static Cpu> {
    (0..count()).filter_map(cpu).find(|c| c.apic_id.load(Ordering::Acquire) == id)
}

/// Stops every other CPU (a panic): each halts in its NMI handler. A second
/// CPU to panic halts here; the first one's screen stands.
pub fn stop_others() {
    if count() <= 1 {
        return;
    }
    if STOPPING.swap(true, Ordering::SeqCst) {
        cpu::halt_forever();
    }
    let me = apic::id();
    for c in (0..count()).filter_map(cpu) {
        let id = c.apic_id.load(Ordering::Acquire);
        if c.online.load(Ordering::Acquire) && id != me && id != NO_APIC {
            apic::send_nmi(id);
        }
    }
}

/// Every NMI: a stop halts here; shootdowns are done. An NMI nobody asked for
/// (a latched second one, or the platform's) is let go.
pub fn on_nmi() {
    if STOPPING.load(Ordering::SeqCst) {
        cpu::halt_forever();
    }
    if let Some(c) = by_apic(apic::id()) {
        tlb::serve(c);
    }
}

crate::ktest!(smp_every_cpu_in_the_madt_is_online, {
    let want = crate::acpi().map_or(1, |t| t.madt.cpu_count);
    if online() != want {
        return Err("not every CPU in the MADT came online");
    }
    let mut ids: Vec<u32> = (0..count()).filter_map(cpu).map(|c| c.apic_id.load(Ordering::Acquire)).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != want || ids.contains(&NO_APIC) {
        return Err("two CPUs share an APIC ID, or one never said its own");
    }
    if crate::timeline::get(crate::timeline::K3) == 0 {
        return Err("no K3 stamp");
    }
    Ok(())
});

crate::ktest!(smp_a_thread_runs_on_every_cpu_and_finds_its_own_block, {
    use core::sync::atomic::AtomicU64 as A;
    static SEEN: A = A::new(0);
    static DONE: AtomicUsize = AtomicUsize::new(0);
    extern "C" fn probe(want: u64) {
        let _irq = cpu::IrqOff::new();
        let c = this();
        // GS's block and the APIC say the same CPU, and it's the one asked for.
        if c.index as u64 == want && c.apic_id.load(Ordering::Acquire) == apic::id() {
            SEEN.fetch_or(1 << want, Ordering::SeqCst);
        }
        DONE.fetch_add(1, Ordering::SeqCst);
    }
    SEEN.store(0, Ordering::SeqCst);
    DONE.store(0, Ordering::SeqCst);
    let n = online();
    for i in 0..n {
        let t = crate::sched::create("smp-probe", crate::sched::Class::Normal, probe, i as u64, false)?;
        crate::sched::pin(&t, i);
        crate::sched::start(&t);
    }
    let until = cpu::rdtsc() + crate::time::ns_to_ticks(2_000_000_000);
    while DONE.load(Ordering::SeqCst) < n && cpu::rdtsc() < until {
        crate::sched::sleep_ns(1_000_000);
    }
    let all = if n == 64 { u64::MAX } else { (1u64 << n) - 1 };
    if SEEN.load(Ordering::SeqCst) != all {
        return Err("a thread pinned to a CPU didn't run there, or found another CPU's block through GS");
    }
    Ok(())
});
