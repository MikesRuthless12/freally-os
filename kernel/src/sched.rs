// SPDX-License-Identifier: Apache-2.0
//! Threads and the scheduler (TASK-026), on every CPU (TASK-036).
//!
//! - **Classes:** realtime, interactive, normal, background; each has its own
//!   queue and time slice (2, 4, 10, 20 ms). The highest non-empty class runs,
//!   round robin within it.
//! - **Every CPU** has its own queues and idle thread. A CPU with nothing of
//!   a class takes one from the CPU with the most of it (work stealing, from
//!   the back: the most recently queued). A woken thread goes to an idle CPU
//!   (the one it last ran on first), else back where it ran; a CPU that should
//!   reschedule for it is sent an IPI. One lock covers every queue for now.
//! - **Preemption:** the TSC-deadline timer ends the slice (or wakes the next
//!   sleeper, whichever is sooner). Each CPU arms its own.
//! - **Interactive wake boost:** an interactive thread that wakes goes to the
//!   front of its queue and preempts lower classes at once.
//! - **Waits never lose a wake:** a thread marks itself blocked
//!   (`prepare_wait`) before it checks what it waits for, so a wake from
//!   another CPU between the check and the sleep finds it blocked and makes it
//!   runnable; `wait` then doesn't sleep. A thread preempted while preparing
//!   stays queued (blocked) and finishes its wait when it runs again.
//! - **Switching:** a thread's registers are saved by the CPU leaving it,
//!   which clears its `on_cpu` only after; another CPU picking it up waits for
//!   that before loading them.
//! - **Kernel stacks** live in their own region with an unmapped guard page
//!   below each, so an overflow faults instead of corrupting memory; a freed
//!   one is shot down from every CPU's TLB before its frames go back.
//! - **FPU/SIMD state** is the user's only (the kernel is soft-float): XSAVE on
//!   the way out of a user thread, XRSTOR on the way in.

use crate::arch::{apic, cpu, gdt};
use crate::mm::{frames, paging};
use crate::obj::{Header, signal};
use crate::smp;
use crate::sync::SpinLock;
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Class {
    Realtime = 0,
    Interactive = 1,
    Normal = 2,
    Background = 3,
}

impl Class {
    pub fn from_u64(v: u64) -> Option<Class> {
        Some(match v {
            0 => Class::Realtime,
            1 => Class::Interactive,
            2 => Class::Normal,
            3 => Class::Background,
            _ => return None,
        })
    }
    fn slice_ns(self) -> u64 {
        [2_000_000, 4_000_000, 10_000_000, 20_000_000][self as usize]
    }
}

/// Runnable: running, or on a queue.
const READY: u8 = 0;
/// Waiting (or about to: see `prepare_wait`).
const BLOCKED: u8 = 2;
const DEAD: u8 = 3;

/// No CPU (a thread not running, unpinned, or that never ran).
const NONE: u32 = u32::MAX;

/// The kernel stack region: 64 KiB stacks, each with a 4 KiB unmapped guard below.
const KSTACK_REGION: u64 = 0xFFFF_C000_0000_0000;
const KSTACK_SIZE: u64 = 64 * 1024;
const KSTACK_SLOT: u64 = 128 * 1024;

pub struct Thread {
    pub id: u64,
    pub name: [u8; 32],
    pub header: Header,
    class: AtomicU8,
    state: AtomicU8,
    saved_rsp: UnsafeCell<u64>,
    stack_slot: Option<u64>,
    pub kstack_top: u64,
    fpu: Option<UnsafeCell<Box<[u8]>>>,
    pub wake_at: AtomicU64,
    /// The TSC ticks it has run (Monitor's CPU %).
    pub run_tsc: AtomicU64,
    /// Killed: it runs only to unwind, and ends on its way back to user mode.
    dying: AtomicBool,
    /// The process this thread belongs to (None for kernel threads).
    pub process: SpinLock<Option<crate::obj::ObjectRef>>,
    /// Its registers are live on a CPU: set as one switches to it, cleared by
    /// that CPU once it has saved them on the way out.
    on_cpu: AtomicBool,
    /// The scheduler's (under its lock): the CPU it's current on, whether
    /// it's on a queue, the CPU it last ran on, the CPU it's pinned to.
    cpu: AtomicU32,
    queued: AtomicBool,
    last_cpu: AtomicU32,
    pin: AtomicU32,
}

// SAFETY: `saved_rsp` and `fpu` are touched only by the scheduler, with
// interrupts off, for the thread that's switching out or in; `on_cpu` keeps
// two CPUs from doing that at once.
unsafe impl Sync for Thread {}
// SAFETY: as above.
unsafe impl Send for Thread {}

pub type ThreadRef = Arc<Thread>;

impl Thread {
    fn new(name: &str, class: Class, state: u8, saved_rsp: u64, stack: Option<(u64, u64)>, user: bool) -> Thread {
        Thread {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            name: name32(name),
            header: Header::new(),
            class: AtomicU8::new(class as u8),
            state: AtomicU8::new(state),
            saved_rsp: UnsafeCell::new(saved_rsp),
            stack_slot: stack.map(|s| s.0),
            kstack_top: stack.map_or(0, |s| s.1),
            fpu: new_fpu_area(user),
            wake_at: AtomicU64::new(0),
            run_tsc: AtomicU64::new(0),
            dying: AtomicBool::new(false),
            process: SpinLock::new(None),
            on_cpu: AtomicBool::new(false),
            cpu: AtomicU32::new(NONE),
            queued: AtomicBool::new(false),
            last_cpu: AtomicU32::new(NONE),
            pin: AtomicU32::new(NONE),
        }
    }
    pub fn class(&self) -> Class {
        Class::from_u64(self.class.load(Ordering::Relaxed) as u64).unwrap_or(Class::Normal)
    }
    pub fn set_class(&self, c: Class) {
        self.class.store(c as u8, Ordering::Relaxed);
    }
    pub fn is_dead(&self) -> bool {
        self.state.load(Ordering::Acquire) == DEAD
    }
    pub fn name(&self) -> &str {
        let n = self.name.iter().position(|&b| b == 0).unwrap_or(32);
        core::str::from_utf8(&self.name[..n]).unwrap_or("?")
    }
}

/// One CPU's part of the scheduler.
struct CpuQ {
    online: bool,
    queues: [VecDeque<ThreadRef>; 4],
    current: Option<ThreadRef>,
    idle: Option<ThreadRef>,
    /// When the running thread was switched in (TSC), and when its slice ends.
    since: u64,
    slice_end: u64,
}

impl CpuQ {
    const fn new() -> CpuQ {
        CpuQ {
            online: false,
            queues: [VecDeque::new(), VecDeque::new(), VecDeque::new(), VecDeque::new()],
            current: None,
            idle: None,
            since: 0,
            slice_end: 0,
        }
    }
    fn queued(&self) -> usize {
        self.queues.iter().map(VecDeque::len).sum()
    }
    /// The running thread's class; 4 when it's the idle thread.
    fn running_class(&self) -> u8 {
        match (&self.current, &self.idle) {
            (Some(c), Some(i)) if Arc::ptr_eq(c, i) => 4,
            (Some(c), _) => c.class() as u8,
            (None, _) => 4,
        }
    }
    fn is_idle(&self) -> bool {
        self.online && self.running_class() == 4 && self.queued() == 0
    }
}

struct Sched {
    cpus: Vec<CpuQ>,
    sleepers: Vec<ThreadRef>,
    zombies: Vec<ThreadRef>,
    free_slots: Vec<u64>,
    next_slot: u64,
}

impl Sched {
    fn online(&self, i: usize) -> bool {
        i < self.cpus.len() && self.cpus[i].online
    }
}

static SCHED: SpinLock<Sched> = SpinLock::new(Sched {
    cpus: Vec::new(),
    sleepers: Vec::new(),
    zombies: Vec::new(),
    free_slots: Vec::new(),
    next_slot: 0,
});
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static STARTED: AtomicBool = AtomicBool::new(false);
static XSAVE_SIZE: AtomicU64 = AtomicU64::new(512);
static USE_XSAVE: AtomicBool = AtomicBool::new(false);

/// This CPU's index. Interrupts off.
fn here() -> usize {
    smp::this().index as usize
}

/// Runs `f` (a write that wakes the thread the caller is about to yield to)
/// without the wake's forced preemption, and with the woken thread queued on
/// this CPU: the caller yields next, and it runs here.
pub fn handoff<R>(f: impl FnOnce() -> R) -> R {
    let _irq = cpu::IrqOff::new();
    let c = smp::this();
    c.handoff.store(true, Ordering::Relaxed);
    let r = f();
    c.handoff.store(false, Ordering::Relaxed);
    r
}

/// Arms this CPU's timer for `deadline` unless it's already armed sooner and
/// still ahead (each MSR write is a VM exit; skip repeats).
fn arm(deadline: u64, now: u64) {
    let c = smp::this();
    let armed = c.armed.load(Ordering::Relaxed);
    if armed > now && armed <= deadline {
        return;
    }
    c.armed.store(deadline, Ordering::Relaxed);
    apic::arm_deadline(deadline.max(now + 1));
}

/// Fires this CPU's timer now (to preempt at once).
fn arm_now() {
    smp::this().armed.store(0, Ordering::Relaxed);
    apic::arm_deadline(cpu::rdtsc() + 1);
}

core::arch::global_asm!(
    // switch_context(old: *mut u64, new: u64, old_on_cpu: *mut bool): saves
    // the callee-saved registers on the old stack and its RSP, says the old
    // thread is off this CPU (nothing touches its stack after), and resumes
    // the new one.
    ".global switch_context",
    "switch_context:",
    "    push rbp",
    "    push rbx",
    "    push r12",
    "    push r13",
    "    push r14",
    "    push r15",
    "    mov [rdi], rsp",
    "    mov byte ptr [rdx], 0",
    "    mov rsp, rsi",
    "    pop r15",
    "    pop r14",
    "    pop r13",
    "    pop r12",
    "    pop rbx",
    "    pop rbp",
    "    ret",
    // A new thread's first return lands here, with its entry in r12 and its argument in r13.
    ".global thread_start",
    "thread_start:",
    "    sti",
    "    mov rdi, r13",
    "    call r12",
    "    call {exit}",
    "    ud2",
    exit = sym exit_current,
);

unsafe extern "C" {
    fn switch_context(old: *mut u64, new: u64, old_on_cpu: *const AtomicBool);
    fn thread_start();
}

fn name32(s: &str) -> [u8; 32] {
    let mut n = [0u8; 32];
    let b = s.as_bytes();
    n[..b.len().min(32)].copy_from_slice(&b[..b.len().min(32)]);
    n
}

/// Turns on the FPU/SIMD state the CPU has (x87, SSE, AVX) for user threads.
/// Every CPU, as it starts.
fn init_fpu() {
    let c1 = cpu::cpuid(1, 0);
    let mut cr4 = cpu::cr4() | (1 << 9) | (1 << 10); // OSFXSR, OSXMMEXCPT
    let xsave = c1.ecx & (1 << 26) != 0;
    if xsave {
        cr4 |= 1 << 18; // OSXSAVE
    }
    // SAFETY: only bits the CPU reports.
    unsafe { cpu::set_cr4(cr4) };
    // SAFETY: CR0.MP on, EM off: FPU instructions run natively.
    unsafe { cpu::set_cr0((cpu::cr0() | (1 << 1)) & !(1 << 2)) };
    if xsave {
        let supported = cpu::cpuid(0xD, 0);
        let mask = (supported.eax as u64) & 0x7;
        // SAFETY: XCR0 gets only the components the CPU supports (x87 always).
        unsafe {
            core::arch::asm!("xsetbv", in("ecx") 0u32, in("eax") mask as u32 | 1, in("edx") 0u32, options(nostack))
        };
        XSAVE_SIZE.store(cpu::cpuid(0xD, 0).ebx as u64, Ordering::Relaxed);
        USE_XSAVE.store(true, Ordering::Relaxed);
    }
}

fn alloc_stack(s: &mut Sched) -> Result<(u64, u64), &'static str> {
    let slot = s.free_slots.pop().unwrap_or_else(|| {
        s.next_slot += 1;
        s.next_slot - 1
    });
    let bottom = KSTACK_REGION + slot * KSTACK_SLOT + 4096; // the guard page stays unmapped
    for p in 0..KSTACK_SIZE / 4096 {
        let f = frames::alloc_zeroed().ok_or("out of memory for a kernel stack")?;
        paging::map(paging::kernel_pml4(), bottom + p * 4096, f, paging::WRITABLE | paging::NX | paging::GLOBAL)?;
    }
    Ok((slot, bottom + KSTACK_SIZE))
}

fn free_stack(s: &mut Sched, slot: u64) {
    let bottom = KSTACK_REGION + slot * KSTACK_SLOT + 4096;
    let mut freed = Vec::new();
    for p in 0..KSTACK_SIZE / 4096 {
        if let Some(f) = paging::unmap(paging::kernel_pml4(), bottom + p * 4096) {
            freed.push(f);
        }
    }
    // No CPU may keep the stack's pages in its TLB once their frames are reused.
    smp::tlb::kernel(bottom, KSTACK_SIZE / 4096);
    for f in freed {
        frames::free(f);
    }
    s.free_slots.push(slot);
}

/// A kernel stack of its own, outside any thread (an AP's boot stack, a
/// test's page); its top.
pub fn alloc_kernel_stack() -> Result<u64, &'static str> {
    alloc_stack(&mut SCHED.lock()).map(|(_, top)| top)
}

/// Gives back a stack from `alloc_kernel_stack`.
pub fn free_kernel_stack(top: u64) {
    let slot = (top - KSTACK_SIZE - 4096 - KSTACK_REGION) / KSTACK_SLOT;
    free_stack(&mut SCHED.lock(), slot);
}

fn new_fpu_area(user: bool) -> Option<UnsafeCell<Box<[u8]>>> {
    if !user {
        return None;
    }
    // A zeroed XSAVE header means "initial state" to XRSTOR; the legacy area's
    // MXCSR gets its reset value.
    let size = XSAVE_SIZE.load(Ordering::Relaxed) as usize + 64;
    let mut b = alloc::vec![0u8; size].into_boxed_slice();
    let off = (64 - (b.as_ptr() as usize % 64)) % 64;
    b[off + 24..off + 28].copy_from_slice(&0x1F80u32.to_le_bytes());
    b[off..off + 2].copy_from_slice(&0x037Fu16.to_le_bytes()); // FCW
    Some(UnsafeCell::new(b))
}

fn fpu_ptr(t: &Thread) -> Option<*mut u8> {
    t.fpu.as_ref().map(|c| {
        // SAFETY: the scheduler is the only user of the area.
        let b = unsafe { &mut *c.get() };
        let off = (64 - (b.as_ptr() as usize % 64)) % 64;
        // SAFETY: inside the box.
        unsafe { b.as_mut_ptr().add(off) }
    })
}

/// Runs `f` with the running thread's FPU and vector state saved around it
/// (firmware runtime calls may use SSE; the kernel itself never does).
pub fn with_fpu_saved<R>(f: impl FnOnce() -> R) -> R {
    let t = current();
    if let Some(t) = &t {
        save_fpu(t);
    }
    let r = f();
    if let Some(t) = &t {
        restore_fpu(t);
    }
    r
}

fn save_fpu(t: &Thread) {
    if let Some(p) = fpu_ptr(t) {
        // SAFETY: a 64-byte aligned area of XSAVE_SIZE (or 512 for FXSAVE) bytes.
        unsafe {
            if USE_XSAVE.load(Ordering::Relaxed) {
                core::arch::asm!("xsave64 [{}]", in(reg) p, in("eax") u32::MAX, in("edx") u32::MAX, options(nostack));
            } else {
                core::arch::asm!("fxsave64 [{}]", in(reg) p, options(nostack));
            }
        }
    }
}

fn restore_fpu(t: &Thread) {
    if let Some(p) = fpu_ptr(t) {
        // SAFETY: as above.
        unsafe {
            if USE_XSAVE.load(Ordering::Relaxed) {
                core::arch::asm!("xrstor64 [{}]", in(reg) p, in("eax") u32::MAX, in("edx") u32::MAX, options(nostack));
            } else {
                core::arch::asm!("fxrstor64 [{}]", in(reg) p, options(nostack));
            }
        }
    }
}

/// Creates a thread that runs `entry(arg)` on its own kernel stack; not started yet.
pub fn create(
    name: &str,
    class: Class,
    entry: extern "C" fn(u64),
    arg: u64,
    user: bool,
) -> Result<ThreadRef, &'static str> {
    let (slot, top) = alloc_stack(&mut SCHED.lock())?;
    // The first switch "returns" into thread_start with r12 = entry, r13 = arg.
    let frame = top - 8 - 6 * 8;
    // SAFETY: the stack was just mapped; write the six saved registers and the return address.
    unsafe {
        let p = frame as *mut u64;
        *p.add(0) = 0; // r15
        *p.add(1) = 0; // r14
        *p.add(2) = arg; // r13
        *p.add(3) = entry as *const () as u64; // r12
        *p.add(4) = 0; // rbx
        *p.add(5) = 0; // rbp
        *p.add(6) = thread_start as *const () as u64;
    }
    Ok(Arc::new(Thread::new(name, class, BLOCKED, frame, Some((slot, top)), user)))
}

/// Keeps a thread (not started yet) on CPU `cpu`.
pub fn pin(t: &ThreadRef, cpu: usize) {
    t.pin.store(cpu as u32, Ordering::Relaxed);
}

/// Keeps the running thread on CPU `cpu` from now on, moving it there first;
/// false if that CPU isn't online.
pub fn pin_current(cpu: usize) -> bool {
    let Some(t) = current() else { return false };
    if !SCHED.lock().online(cpu) {
        return false;
    }
    t.pin.store(cpu as u32, Ordering::Relaxed);
    drop(t);
    // Each switch requeues it on its pinned CPU.
    loop {
        let at = {
            let _irq = cpu::IrqOff::new();
            here()
        };
        if at == cpu {
            return true;
        }
        switch(false);
    }
}

/// Puts a runnable thread on a queue: its pinned CPU's; here on a handoff;
/// else an idle CPU (the one it last ran on first); else back where it last
/// ran. Returns the CPU that should reschedule for it, if one should.
fn enqueue(s: &mut Sched, t: ThreadRef, here: usize, handoff: bool) -> Option<usize> {
    let n = s.cpus.len();
    let pin = t.pin.load(Ordering::Relaxed) as usize;
    let last = t.last_cpu.load(Ordering::Relaxed) as usize;
    let target = if s.online(pin) {
        pin
    } else if handoff {
        here
    } else if s.online(last) && s.cpus[last].is_idle() {
        last
    } else if let Some(i) = (0..n).map(|k| (here + k) % n).find(|&i| s.cpus[i].is_idle()) {
        i
    } else if s.online(last) {
        last
    } else {
        here
    };
    let c = t.class();
    t.queued.store(true, Ordering::Relaxed);
    let q = &mut s.cpus[target];
    let preempt = q.running_class() > c as u8;
    if c == Class::Interactive || c == Class::Realtime {
        q.queues[c as usize].push_front(t);
    } else {
        q.queues[c as usize].push_back(t);
    }
    preempt.then_some(target)
}

/// After a thread went BLOCKED → READY: queue it, unless it's some CPU's
/// current (it's running, and will find it's runnable) or already queued
/// (preempted while preparing to wait).
fn ready(s: &mut Sched, t: &ThreadRef, here: usize, handoff: bool) -> Option<usize> {
    s.sleepers.retain(|x| !Arc::ptr_eq(x, t));
    if t.queued.load(Ordering::Relaxed) || t.cpu.load(Ordering::Relaxed) != NONE {
        return None;
    }
    enqueue(s, t.clone(), here, handoff)
}

/// Has CPU `k` reschedule: this one at once (unless a handoff's yield is
/// coming anyway), another by IPI. Interrupts off.
fn kick(k: Option<usize>, here: usize, handoff: bool) {
    match k {
        Some(k) if k == here => {
            smp::this().need_resched.store(true, Ordering::Relaxed);
            if !handoff {
                arm_now();
            }
        }
        Some(k) => smp::resched(k),
        None => {}
    }
}

/// Makes a created thread runnable (once: a thread killed before it started
/// is already on its way to its end).
pub fn start(t: &ThreadRef) {
    if t.state.compare_exchange(BLOCKED, READY, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
        let _irq = cpu::IrqOff::new();
        let (here, k) = {
            let mut s = SCHED.lock();
            let here = here();
            (here, ready(&mut s, t, here, false))
        };
        kick(k, here, false);
    }
}

/// Whether the running thread has been killed.
pub fn dying() -> bool {
    let _irq = cpu::IrqOff::new();
    let t = smp::this().current.load(Ordering::Acquire);
    // SAFETY: the running thread is alive: this CPU's queue holds it as current.
    !t.is_null() && unsafe { (*t).dying.load(Ordering::SeqCst) }
}

extern "C" fn idle_loop(_: u64) {
    loop {
        cpu::interrupts_on();
        // SAFETY: waits for the next interrupt (`sti` holds it off until `hlt`).
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
}

/// Wraps the boot code as thread 0 and creates the boot CPU's idle thread.
pub fn init() -> Result<(), &'static str> {
    init_fpu();
    let boot = Arc::new(Thread::new("boot", Class::Normal, READY, 0, None, false));
    let idle = create("idle", Class::Background, idle_loop, 0, false)?;
    idle.state.store(READY, Ordering::Release);
    become_current(0, boot, idle);
    STARTED.store(true, Ordering::Release);
    Ok(())
}

/// The running code is now `t`, CPU `i`'s current, with `idle` its idle thread.
fn become_current(i: usize, t: ThreadRef, idle: ThreadRef) {
    let _irq = cpu::IrqOff::new();
    let mut s = SCHED.lock();
    if s.cpus.len() <= i {
        s.cpus.resize_with(i + 1, CpuQ::new);
    }
    t.on_cpu.store(true, Ordering::Release);
    t.cpu.store(i as u32, Ordering::Relaxed);
    t.last_cpu.store(i as u32, Ordering::Relaxed);
    smp::this().current.store(Arc::as_ptr(&t) as *mut Thread, Ordering::Release);
    let q = &mut s.cpus[i];
    q.online = true;
    q.since = cpu::rdtsc();
    q.current = Some(t);
    q.idle = Some(idle);
}

/// Room in the scheduler for `n` CPUs (offline until each starts).
pub fn add_cpus(n: usize) {
    let mut s = SCHED.lock();
    if s.cpus.len() < n {
        s.cpus.resize_with(n, CpuQ::new);
    }
}

/// An AP, set up: its boot code becomes its idle thread, and it waits for work.
pub fn run_ap(i: usize) -> ! {
    init_fpu();
    let me = Arc::new(Thread::new("idle", Class::Background, READY, 0, None, false));
    become_current(i, me.clone(), me);
    smp::mark_online(smp::this());
    idle_loop(0);
    unreachable!("the idle loop returned");
}

pub fn current() -> Option<ThreadRef> {
    let _irq = cpu::IrqOff::new();
    let p = smp::this().current.load(Ordering::Acquire);
    if p.is_null() {
        return None;
    }
    // SAFETY: the running thread is alive (this CPU's queue holds it as
    // current), and only this CPU, with interrupts off here, replaces it.
    unsafe {
        Arc::increment_strong_count(p);
        Some(Arc::from_raw(p))
    }
}

/// The next thread for CPU `here`: the highest class with one queued, its own
/// queue first, else the CPU with the most of that class (an unpinned one,
/// from the back).
fn pick(s: &mut Sched, here: usize) -> Option<ThreadRef> {
    let n = s.cpus.len();
    for c in 0..4 {
        if let Some(t) = s.cpus[here].queues[c].pop_front() {
            return Some(t);
        }
        let victim = (0..n).filter(|&i| i != here && s.cpus[i].online).max_by_key(|&i| s.cpus[i].queues[c].len());
        if let Some(v) = victim {
            let q = &mut s.cpus[v].queues[c];
            if let Some(at) = q.iter().rposition(|t| t.pin.load(Ordering::Relaxed) == NONE) {
                return q.remove(at);
            }
        }
    }
    None
}

/// Picks the next thread and switches to it (a thread's own yield or wait).
pub fn schedule() {
    switch(false);
}

/// The same, taken from the running thread (its slice ended, or something
/// more urgent woke): it stays runnable even halfway into a wait.
pub fn preempt() {
    switch(true);
}

/// Another CPU asked this one to reschedule.
pub fn resched_interrupt(_f: &mut crate::arch::idt::Frame) {
    apic::eoi();
    preempt();
}

fn switch(preempted: bool) {
    if !STARTED.load(Ordering::Acquire) {
        return;
    }
    let irq_on = cpu::interrupts_enabled();
    cpu::interrupts_off();
    let me = smp::this();
    me.need_resched.store(false, Ordering::Relaxed);
    let here = me.index as usize;
    let (prev, next, reaped, kick_cpu) = {
        let mut s = SCHED.lock();
        // Free the stacks of the dead that no CPU still stands on. The threads
        // themselves are dropped after the lock is released: the last reference
        // to a process closes its handles, which can wake other threads.
        let mut reaped = Vec::new();
        let mut i = 0;
        while i < s.zombies.len() {
            if s.zombies[i].on_cpu.load(Ordering::Acquire) {
                i += 1;
            } else {
                reaped.push(s.zombies.swap_remove(i));
            }
        }
        for z in &reaped {
            if let Some(slot) = z.stack_slot {
                free_stack(&mut s, slot);
            }
        }
        let n = s.cpus.len();
        let prev = s.cpus[here].current.clone().expect("a current thread");
        let is_idle = s.cpus[here].idle.as_ref().is_some_and(|i| Arc::ptr_eq(i, &prev));
        let state = prev.state.load(Ordering::SeqCst);
        let mut kick_cpu = None;
        // Still runnable, or preempted halfway into a wait: back on a queue
        // (its pinned CPU's, if it has one).
        let requeued = !is_idle && (state == READY || (preempted && state == BLOCKED));
        if requeued {
            let pin = prev.pin.load(Ordering::Relaxed) as usize;
            let to = if s.online(pin) { pin } else { here };
            let c = prev.class() as usize;
            prev.queued.store(true, Ordering::Relaxed);
            s.cpus[to].queues[c].push_back(prev.clone());
            if to != here && s.cpus[to].running_class() > c as u8 {
                kick_cpu = Some(to);
            }
        }
        let next = pick(&mut s, here).or_else(|| s.cpus[here].idle.clone()).expect("the idle thread");
        next.queued.store(false, Ordering::Relaxed);
        if !Arc::ptr_eq(&prev, &next) {
            prev.cpu.store(NONE, Ordering::Relaxed);
        }
        next.cpu.store(here as u32, Ordering::Relaxed);
        next.last_cpu.store(here as u32, Ordering::Relaxed);
        me.current.store(Arc::as_ptr(&next) as *mut Thread, Ordering::Release);
        let now = cpu::rdtsc();
        let q = &mut s.cpus[here];
        // The time prev ran, to its account.
        if q.since != 0 {
            prev.run_tsc.fetch_add(now.saturating_sub(q.since), Ordering::Relaxed);
        }
        q.since = now;
        q.slice_end = now + crate::time::ns_to_ticks(next.class().slice_ns());
        q.current = Some(next.clone());
        let slice_end = q.slice_end;
        // Work left waiting here while another CPU idles: wake that one to
        // take it (not on a yield: the yielder runs again soon, here).
        if kick_cpu.is_none() && (preempted || !requeued) && s.cpus[here].queued() > 0 {
            kick_cpu = (0..n).find(|&i| i != here && s.cpus[i].is_idle());
        }
        let wake = s.sleepers.iter().map(|t| t.wake_at.load(Ordering::Relaxed)).min().unwrap_or(u64::MAX);
        arm(slice_end.min(wake), now);
        (prev, next, reaped, kick_cpu)
    };
    if let Some(k) = kick_cpu {
        smp::resched(k);
    }
    drop(reaped);
    if !Arc::ptr_eq(&prev, &next) {
        // `next` may still be on its way off the CPU it last ran on: wait for
        // that CPU to have saved its registers.
        while next.on_cpu.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }
        next.on_cpu.store(true, Ordering::Relaxed);
        save_fpu(&prev);
        if next.kstack_top != 0 {
            gdt::set_kernel_stack(next.kstack_top);
            crate::syscall::set_kernel_rsp(next.kstack_top);
        }
        // The address space: the thread's process's, else the kernel's (CR3 is
        // written only when it changes: each write is a VM exit and a TLB flush).
        if let Some(p) = next.process.lock().as_ref().and_then(|p| crate::proc::as_process(p).ok()) {
            let io = p.io.lock();
            crate::hw::io_switch(p.pid, &io);
        } else {
            crate::hw::io_switch(0, &[]);
        }
        let want = next.process.lock().as_ref().and_then(crate::proc::aspace_of);
        let want_pml4 = want.as_ref().map_or(paging::kernel_pml4(), |a| a.pml4);
        if cpu::cr3() & !0xFFF != want_pml4 {
            match &want {
                Some(a) => a.activate(),
                None => crate::mm::aspace::activate_kernel(),
            }
        }
        drop(want);
        restore_fpu(&next);
        let save = prev.saved_rsp.get();
        // SAFETY: the scheduler owns `saved_rsp`; `next`'s was saved (on_cpu was clear).
        let to = unsafe { *next.saved_rsp.get() };
        let leaving = &prev.on_cpu as *const AtomicBool;
        // No reference may stay on this stack across the switch: a dead
        // thread's stack never resumes, so whatever it held would leak (its
        // process with it). The zombie list keeps a dead `prev` alive until
        // it's reaped (after `on_cpu` clears); `current` keeps `next`.
        drop(next);
        let keep = (prev.state.load(Ordering::Acquire) != DEAD).then_some(prev);
        // SAFETY: both stacks are valid; `leaving` lives as long as `prev` does.
        unsafe { switch_context(save, to, leaving) };
        drop(keep);
    }
    if irq_on {
        cpu::interrupts_on();
    }
}

/// The timer: wake sleepers whose time has come; end the slice if it's over.
pub fn tick() {
    let now = cpu::rdtsc();
    let me = smp::this();
    let here = me.index as usize;
    let mut resched = me.need_resched.load(Ordering::Relaxed);
    let mut others = 0u64;
    {
        let mut s = SCHED.lock();
        let mut i = 0;
        while i < s.sleepers.len() {
            if s.sleepers[i].wake_at.load(Ordering::Relaxed) <= now {
                let t = s.sleepers.swap_remove(i);
                if t.state.compare_exchange(BLOCKED, READY, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                    match ready(&mut s, &t, here, false) {
                        Some(k) if k == here => resched = true,
                        Some(k) => others |= 1 << k,
                        None => {}
                    }
                }
            } else {
                i += 1;
            }
        }
        if now >= s.cpus[here].slice_end {
            resched = true;
        }
        if !resched {
            let wake = s.sleepers.iter().map(|t| t.wake_at.load(Ordering::Relaxed)).min().unwrap_or(u64::MAX);
            arm(s.cpus[here].slice_end.min(wake), now);
        }
    }
    for k in (0..smp::MAX_CPUS).filter(|k| others & (1 << k) != 0) {
        smp::resched(k);
    }
    if resched {
        preempt();
    }
}

/// Wakes a blocked thread. An interactive or realtime thread jumps its queue
/// and preempts lower classes at the next chance.
pub fn wake(t: &ThreadRef) {
    if t.state.compare_exchange(BLOCKED, READY, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return;
    }
    let _irq = cpu::IrqOff::new();
    let c = smp::this();
    let (here, handoff) = (c.index as usize, c.handoff.load(Ordering::Relaxed));
    let k = ready(&mut SCHED.lock(), t, here, handoff);
    kick(k, here, handoff);
}

/// The first half of a wait: from here on, a `wake` isn't lost. Call it
/// before checking what the thread waits for, then [`wait`] (or
/// [`cancel_wait`] if it's there already).
pub fn prepare_wait() {
    if let Some(t) = current() {
        t.state.store(BLOCKED, Ordering::SeqCst);
    }
}

/// What a prepared wait waited for was there already: runnable again. (A
/// wake may have got here first; either way it's runnable, and the running
/// thread is on no queue.)
pub fn cancel_wait() {
    if let Some(t) = current() {
        let _ = t.state.compare_exchange(BLOCKED, READY, Ordering::SeqCst, Ordering::SeqCst);
    }
}

/// The second half: sleeps until woken, or until `deadline` in TSC ticks (if
/// not u64::MAX). Returns at once if a wake came after `prepare_wait`. A
/// killed thread never sleeps: callers see `dying()` and unwind.
pub fn wait(deadline: u64) {
    let Some(t) = current() else { return };
    if t.dying.load(Ordering::SeqCst) {
        cancel_wait();
        return;
    }
    if deadline != u64::MAX {
        t.wake_at.store(deadline, Ordering::Relaxed);
        let mut s = SCHED.lock();
        // Under the lock: a wake either came before (it's READY, no sleeper)
        // or comes after (and takes it off the list).
        if t.state.load(Ordering::SeqCst) == BLOCKED {
            s.sleepers.push(t.clone());
        }
    }
    drop(t);
    switch(false);
}

/// Blocks the current thread until `wake` (or until `deadline` in TSC ticks,
/// if not u64::MAX), with nothing to check first.
pub fn block_until(deadline: u64) {
    prepare_wait();
    wait(deadline);
}

pub fn sleep_ns(ns: u64) {
    block_until(cpu::rdtsc() + crate::time::ns_to_ticks(ns));
}

pub fn yield_now() {
    switch(false);
}

/// Ends the current thread. Call only with nothing of the kernel's on the
/// stack (a thread's last return, or on its way back to user mode): what the
/// stack holds is never dropped.
pub extern "C" fn exit_current() -> ! {
    if let Some(t) = current() {
        crate::proc::thread_gone(&t);
        t.state.store(DEAD, Ordering::SeqCst);
        t.header.update(0, signal::TERMINATED);
        SCHED.lock().zombies.push(t);
    }
    switch(false);
    unreachable!("a dead thread was scheduled");
}

crate::ktest!(sched_an_interactive_thread_wakes_within_a_millisecond_under_load, {
    use core::sync::atomic::AtomicU64 as A;
    static STOP: AtomicBool = AtomicBool::new(false);
    static WORST: A = A::new(0);
    static DONE: AtomicBool = AtomicBool::new(false);
    extern "C" fn spin(_: u64) {
        while !STOP.load(Ordering::Relaxed) {
            core::hint::spin_loop();
        }
    }
    extern "C" fn waker(_: u64) {
        for _ in 0..20 {
            let want = cpu::rdtsc() + crate::time::ns_to_ticks(2_000_000);
            block_until(want);
            let late = crate::time::ticks_to_ns(cpu::rdtsc().saturating_sub(want));
            WORST.fetch_max(late, Ordering::Relaxed);
        }
        DONE.store(true, Ordering::Release);
    }
    // Every run starts afresh (the harness runs it on each CPU), and on one
    // CPU: the one under test, loaded with all 32 spinners.
    STOP.store(false, Ordering::Relaxed);
    WORST.store(0, Ordering::Relaxed);
    DONE.store(false, Ordering::Release);
    let on = {
        let _irq = cpu::IrqOff::new();
        here()
    };
    let mut spinners = Vec::new();
    for _ in 0..32 {
        let t = create("spinner", Class::Normal, spin, 0, false)?;
        pin(&t, on);
        start(&t);
        spinners.push(t);
    }
    let w = create("waker", Class::Interactive, waker, 0, false)?;
    pin(&w, on);
    start(&w);
    cpu::interrupts_on();
    while !DONE.load(Ordering::Acquire) {
        sleep_ns(1_000_000);
    }
    STOP.store(true, Ordering::Relaxed);
    while spinners.iter().any(|t| !t.is_dead()) || !w.is_dead() {
        sleep_ns(1_000_000);
    }
    cpu::interrupts_off();
    let worst = WORST.load(Ordering::Relaxed);
    klog!("    worst interactive wake-up with 32 spinners: {} µs", worst / 1000);
    if worst > 1_000_000 { Err("an interactive thread woke more than 1 ms late") } else { Ok(()) }
});

crate::ktest!(sched_a_kernel_stack_has_a_guard_page, {
    extern "C" fn nop(_: u64) {}
    let t = create("probe", Class::Normal, nop, 0, false)?;
    let bottom = t.kstack_top - KSTACK_SIZE;
    if crate::exceptions::try_read(t.kstack_top - 8).is_err() {
        return Err("a kernel stack isn't mapped");
    }
    if crate::exceptions::try_read(bottom - 8) != Err(14) {
        return Err("the page below a kernel stack is mapped (no guard page)");
    }
    if let Some(slot) = t.stack_slot {
        free_stack(&mut SCHED.lock(), slot);
    }
    Ok(())
});

/// Sets the argument a created (not yet started) thread's entry receives.
pub fn set_start_arg(t: &ThreadRef, arg: u64) {
    // SAFETY: the thread hasn't run; its initial frame holds r13 at slot 2.
    unsafe { *((*t.saved_rsp.get()) as *mut u64).add(2) = arg };
}

/// Kills a thread. It isn't stopped where it stands (what its kernel stack
/// holds would never be dropped): woken if blocked, it runs only to unwind
/// (its waits return at once) and ends on its way back to user mode, with
/// nothing of the kernel's left on its stack.
pub fn kill(t: &ThreadRef) {
    t.dying.store(true, Ordering::SeqCst);
    wake(t);
    // Running on another CPU (in user mode, likely): have it notice now, on
    // its way back from the IPI, not at the end of its slice.
    let _irq = cpu::IrqOff::new();
    let on = t.cpu.load(Ordering::Relaxed);
    if on != NONE && on as usize != here() {
        smp::resched(on as usize);
    }
}

/// Kills the running thread: it ends on its way back to user mode.
pub fn kill_current() {
    if let Some(t) = current() {
        kill(&t);
    }
}

crate::ktest!(sched_threads_hopping_between_cpus_keep_their_registers, {
    // Each hop queues the thread on another CPU while this one is still
    // switching away from it: the CPU taking it must wait for its registers.
    const HOPS: u64 = 2000;
    static DONE: AtomicU64 = AtomicU64::new(0);
    static BAD: AtomicU64 = AtomicU64::new(0);
    extern "C" fn hopper(arg: u64) {
        let n = smp::online() as u64;
        let canary = core::hint::black_box(arg.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut sum = 0u64;
        for k in 0..HOPS {
            let to = ((arg + k) % n) as usize;
            let moved = pin_current(to);
            sum = core::hint::black_box(sum.wrapping_add(k));
            let at = {
                let _irq = cpu::IrqOff::new();
                here()
            };
            if !moved || at != to || canary != arg.wrapping_mul(0x9E37_79B9_7F4A_7C15) {
                BAD.fetch_add(1, Ordering::SeqCst);
            }
        }
        if sum != HOPS * (HOPS - 1) / 2 {
            BAD.fetch_add(1, Ordering::SeqCst);
        }
        DONE.fetch_add(1, Ordering::SeqCst);
    }
    let n = smp::online() as u64;
    if n < 2 {
        klog!("    skipped: one CPU (this test needs -smp 2 or more)");
        return Ok(());
    }
    DONE.store(0, Ordering::SeqCst);
    BAD.store(0, Ordering::SeqCst);
    for i in 0..2 * n {
        start(&create("hopper", Class::Normal, hopper, i, false)?);
    }
    let until = cpu::rdtsc() + crate::time::ns_to_ticks(20_000_000_000);
    while DONE.load(Ordering::SeqCst) < 2 * n && cpu::rdtsc() < until {
        sleep_ns(1_000_000);
    }
    klog!("    {} threads, {} hops each, across {} CPUs", 2 * n, HOPS, n);
    if DONE.load(Ordering::SeqCst) < 2 * n {
        Err("hopping threads never finished")
    } else if BAD.load(Ordering::SeqCst) != 0 {
        Err("a thread came back on the wrong CPU or with its stack changed")
    } else {
        Ok(())
    }
});
