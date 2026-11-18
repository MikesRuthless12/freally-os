// SPDX-License-Identifier: Apache-2.0
//! Threads and the scheduler (TASK-026). One CPU until SMP (TASK-036).
//!
//! - **Classes:** realtime, interactive, normal, background; each has its own
//!   queue and time slice (2, 4, 10, 20 ms). The highest non-empty class runs,
//!   round robin within it.
//! - **Preemption:** the TSC-deadline timer ends the slice (or wakes the next
//!   sleeper, whichever is sooner).
//! - **Interactive wake boost:** an interactive thread that wakes goes to the
//!   front of its queue and preempts lower classes at once.
//! - **Kernel stacks** live in their own region with an unmapped guard page
//!   below each, so an overflow faults instead of corrupting memory.
//! - **FPU/SIMD state** is the user's only (the kernel is soft-float): XSAVE on
//!   the way out of a user thread, XRSTOR on the way in.

use crate::arch::{cpu, gdt};
use crate::mm::{frames, paging};
use crate::obj::{Header, signal};
use crate::sync::SpinLock;
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU64, Ordering};

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

const READY: u8 = 0;
const RUNNING: u8 = 1;
const BLOCKED: u8 = 2;
const DEAD: u8 = 3;

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
    /// Killed: it runs only to unwind, and ends on its way back to user mode.
    dying: AtomicBool,
    /// The process this thread belongs to (None for kernel threads).
    pub process: SpinLock<Option<crate::obj::ObjectRef>>,
}

// SAFETY: `saved_rsp` and `fpu` are touched only by the scheduler, with
// interrupts off, for the thread that's switching out or in.
unsafe impl Sync for Thread {}
// SAFETY: as above.
unsafe impl Send for Thread {}

pub type ThreadRef = Arc<Thread>;

impl Thread {
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

struct Sched {
    queues: [VecDeque<ThreadRef>; 4],
    current: Option<ThreadRef>,
    idle: Option<ThreadRef>,
    sleepers: Vec<ThreadRef>,
    zombies: Vec<ThreadRef>,
    slice_end: u64,
    free_slots: Vec<u64>,
    next_slot: u64,
}

static SCHED: SpinLock<Sched> = SpinLock::new(Sched {
    queues: [VecDeque::new(), VecDeque::new(), VecDeque::new(), VecDeque::new()],
    current: None,
    idle: None,
    sleepers: Vec::new(),
    zombies: Vec::new(),
    slice_end: 0,
    free_slots: Vec::new(),
    next_slot: 0,
});
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static NEED_RESCHED: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicBool = AtomicBool::new(false);
static XSAVE_SIZE: AtomicU64 = AtomicU64::new(512);
/// The TSC deadline the timer is armed for (each MSR write is a VM exit; skip repeats).
static ARMED: AtomicU64 = AtomicU64::new(0);

/// Arms the timer for `deadline` unless it's already armed sooner and still ahead.
fn arm(deadline: u64, now: u64) {
    let armed = ARMED.load(Ordering::Relaxed);
    if armed > now && armed <= deadline {
        return;
    }
    ARMED.store(deadline, Ordering::Relaxed);
    crate::arch::apic::arm_deadline(deadline.max(now + 1));
}
static USE_XSAVE: AtomicBool = AtomicBool::new(false);

core::arch::global_asm!(
    // switch_context(old: *mut u64, new: u64): saves the callee-saved registers
    // on the old stack, stores its RSP, and resumes the new one.
    ".global switch_context",
    "switch_context:",
    "    push rbp",
    "    push rbx",
    "    push r12",
    "    push r13",
    "    push r14",
    "    push r15",
    "    mov [rdi], rsp",
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
    fn switch_context(old: *mut u64, new: u64);
    fn thread_start();
}

fn name32(s: &str) -> [u8; 32] {
    let mut n = [0u8; 32];
    let b = s.as_bytes();
    n[..b.len().min(32)].copy_from_slice(&b[..b.len().min(32)]);
    n
}

/// Turns on the FPU/SIMD state the CPU has (x87, SSE, AVX) for user threads.
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
    for p in 0..KSTACK_SIZE / 4096 {
        if let Some(f) = paging::unmap(paging::kernel_pml4(), bottom + p * 4096) {
            frames::free(f);
        }
    }
    s.free_slots.push(slot);
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
    Ok(Arc::new(Thread {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        name: name32(name),
        header: Header::new(),
        class: AtomicU8::new(class as u8),
        state: AtomicU8::new(BLOCKED),
        saved_rsp: UnsafeCell::new(frame),
        stack_slot: Some(slot),
        kstack_top: top,
        fpu: new_fpu_area(user),
        wake_at: AtomicU64::new(0),
        dying: AtomicBool::new(false),
        process: SpinLock::new(None),
    }))
}

/// Makes a created thread runnable (once: a thread killed before it started
/// is already on its way to its end).
pub fn start(t: &ThreadRef) {
    if t.state.compare_exchange(BLOCKED, READY, Ordering::AcqRel, Ordering::Acquire).is_ok() {
        SCHED.lock().queues[t.class() as usize].push_back(t.clone());
    }
}

/// The running thread, for `dying` on every return to user mode without the
/// scheduler's lock. Written only by the scheduler (per CPU with SMP, TASK-036).
static CURRENT: AtomicPtr<Thread> = AtomicPtr::new(core::ptr::null_mut());

/// Whether the running thread has been killed.
pub fn dying() -> bool {
    let t = CURRENT.load(Ordering::Acquire);
    // SAFETY: the running thread is alive: the scheduler's `current` holds it.
    !t.is_null() && unsafe { (*t).dying.load(Ordering::Acquire) }
}

extern "C" fn idle_loop(_: u64) {
    loop {
        cpu::interrupts_on();
        // SAFETY: waits for the next interrupt.
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
}

/// Wraps the boot code as thread 0 and creates the idle thread.
pub fn init() -> Result<(), &'static str> {
    init_fpu();
    let boot = Arc::new(Thread {
        id: 0,
        name: name32("boot"),
        header: Header::new(),
        class: AtomicU8::new(Class::Normal as u8),
        state: AtomicU8::new(RUNNING),
        saved_rsp: UnsafeCell::new(0),
        stack_slot: None,
        kstack_top: 0,
        fpu: None,
        wake_at: AtomicU64::new(0),
        dying: AtomicBool::new(false),
        process: SpinLock::new(None),
    });
    let idle = create("idle", Class::Background, idle_loop, 0, false)?;
    idle.state.store(READY, Ordering::Release);
    let mut s = SCHED.lock();
    CURRENT.store(Arc::as_ptr(&boot) as *mut Thread, Ordering::Release);
    s.current = Some(boot);
    s.idle = Some(idle);
    drop(s);
    STARTED.store(true, Ordering::Release);
    Ok(())
}

pub fn current() -> Option<ThreadRef> {
    SCHED.lock().current.clone()
}

/// Picks the next thread and switches to it. Interrupts are off throughout.
pub fn schedule() {
    if !STARTED.load(Ordering::Acquire) {
        return;
    }
    let irq_on = {
        let f: u64;
        // SAFETY: reads RFLAGS.
        unsafe { core::arch::asm!("pushfq; pop {}", out(reg) f, options(nomem, preserves_flags)) };
        f & (1 << 9) != 0
    };
    cpu::interrupts_off();
    NEED_RESCHED.store(false, Ordering::Relaxed);
    let (prev, next, reaped) = {
        let mut s = SCHED.lock();
        // Free the stacks of threads that died earlier (never the one running now).
        // The threads themselves are dropped after the lock is released: the last
        // reference to a process closes its handles, which can wake other threads.
        let cur_id = s.current.as_ref().map(|t| t.id);
        let zombies: Vec<ThreadRef> = core::mem::take(&mut s.zombies);
        let mut reaped = Vec::new();
        for z in zombies {
            if Some(z.id) == cur_id {
                s.zombies.push(z);
            } else {
                if let Some(slot) = z.stack_slot {
                    free_stack(&mut s, slot);
                }
                reaped.push(z);
            }
        }
        let prev = s.current.clone().expect("a current thread");
        let state = prev.state.load(Ordering::Acquire);
        let is_idle = s.idle.as_ref().is_some_and(|i| Arc::ptr_eq(i, &prev));
        if state == RUNNING && !is_idle {
            prev.state.store(READY, Ordering::Release);
            s.queues[prev.class() as usize].push_back(prev.clone());
        }
        let next = (0..4).find_map(|c| s.queues[c].pop_front()).or_else(|| s.idle.clone()).expect("the idle thread");
        next.state.store(RUNNING, Ordering::Release);
        CURRENT.store(Arc::as_ptr(&next) as *mut Thread, Ordering::Release);
        s.current = Some(next.clone());
        let now = cpu::rdtsc();
        s.slice_end = now + crate::time::ns_to_ticks(next.class().slice_ns());
        let wake = s.sleepers.iter().map(|t| t.wake_at.load(Ordering::Relaxed)).min().unwrap_or(u64::MAX);
        arm(s.slice_end.min(wake), now);
        (prev, next, reaped)
    };
    drop(reaped);
    if !Arc::ptr_eq(&prev, &next) {
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
        // SAFETY: the scheduler owns `saved_rsp`.
        let to = unsafe { *next.saved_rsp.get() };
        // No reference may stay on this stack across the switch: a dead
        // thread's stack never resumes, so whatever it held would leak (its
        // process with it). The zombie list keeps a dead `prev` alive until
        // it's reaped, after this switch; `current` keeps `next`.
        drop(next);
        let keep = (prev.state.load(Ordering::Acquire) != DEAD).then_some(prev);
        // SAFETY: both stacks are valid.
        unsafe { switch_context(save, to) };
        drop(keep);
    }
    if irq_on {
        cpu::interrupts_on();
    }
}

/// The timer: wake sleepers whose time has come; end the slice if it's over.
pub fn tick() {
    let now = cpu::rdtsc();
    let mut resched = NEED_RESCHED.load(Ordering::Relaxed);
    {
        let mut s = SCHED.lock();
        let mut i = 0;
        while i < s.sleepers.len() {
            if s.sleepers[i].wake_at.load(Ordering::Relaxed) <= now {
                let t = s.sleepers.swap_remove(i);
                if t.state.compare_exchange(BLOCKED, READY, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                    let c = t.class();
                    if c == Class::Interactive || c == Class::Realtime {
                        s.queues[c as usize].push_front(t);
                    } else {
                        s.queues[c as usize].push_back(t);
                    }
                    resched = true;
                }
            } else {
                i += 1;
            }
        }
        if now >= s.slice_end {
            resched = true;
        }
        if !resched {
            let wake = s.sleepers.iter().map(|t| t.wake_at.load(Ordering::Relaxed)).min().unwrap_or(u64::MAX);
            arm(s.slice_end.min(wake), now);
        }
    }
    if resched {
        schedule();
    }
}

/// Wakes a blocked thread. An interactive or realtime thread jumps its queue
/// and preempts lower classes at the next chance.
pub fn wake(t: &ThreadRef) {
    if t.state.compare_exchange(BLOCKED, READY, Ordering::AcqRel, Ordering::Acquire).is_err() {
        return;
    }
    let mut s = SCHED.lock();
    s.sleepers.retain(|x| !Arc::ptr_eq(x, t));
    let c = t.class();
    let cur = s.current.as_ref().map_or(Class::Background, |x| x.class());
    if c == Class::Interactive || c == Class::Realtime {
        s.queues[c as usize].push_front(t.clone());
    } else {
        s.queues[c as usize].push_back(t.clone());
    }
    if (c as u8) < (cur as u8) {
        NEED_RESCHED.store(true, Ordering::Relaxed);
        ARMED.store(0, Ordering::Relaxed);
        crate::arch::apic::arm_deadline(cpu::rdtsc() + 1);
    }
}

/// Blocks the current thread until `wake` (or until `deadline` in TSC ticks,
/// if not u64::MAX). A killed thread never blocks: callers see `dying()` and
/// unwind.
pub fn block_until(deadline: u64) {
    let Some(t) = current() else { return };
    if t.dying.load(Ordering::Acquire) {
        return;
    }
    t.state.store(BLOCKED, Ordering::Release);
    // A kill between the check and the store found it running and woke
    // nothing: undo the block. (If its wake got in first, it's queued; the
    // switch below returns to it at once.)
    if t.dying.load(Ordering::Acquire)
        && t.state.compare_exchange(BLOCKED, RUNNING, Ordering::AcqRel, Ordering::Acquire).is_ok()
    {
        return;
    }
    if deadline != u64::MAX {
        t.wake_at.store(deadline, Ordering::Relaxed);
        SCHED.lock().sleepers.push(t.clone());
    }
    schedule();
}

pub fn sleep_ns(ns: u64) {
    block_until(cpu::rdtsc() + crate::time::ns_to_ticks(ns));
}

pub fn yield_now() {
    schedule();
}

/// Ends the current thread. Call only with nothing of the kernel's on the
/// stack (a thread's last return, or on its way back to user mode): what the
/// stack holds is never dropped.
pub extern "C" fn exit_current() -> ! {
    if let Some(t) = current() {
        crate::proc::thread_gone(&t);
        t.state.store(DEAD, Ordering::Release);
        t.header.update(0, signal::TERMINATED);
        SCHED.lock().zombies.push(t);
    }
    schedule();
    unreachable!("a dead thread was scheduled");
}

ktest!(sched_an_interactive_thread_wakes_within_a_millisecond_under_load, {
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
    STOP.store(false, Ordering::Relaxed);
    let mut spinners = Vec::new();
    for _ in 0..32 {
        let t = create("spinner", Class::Normal, spin, 0, false)?;
        start(&t);
        spinners.push(t);
    }
    let w = create("waker", Class::Interactive, waker, 0, false)?;
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

ktest!(sched_a_kernel_stack_has_a_guard_page, {
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
    t.dying.store(true, Ordering::Release);
    wake(t);
}

/// Kills the running thread: it ends on its way back to user mode.
pub fn kill_current() {
    if let Some(t) = current() {
        kill(&t);
    }
}
