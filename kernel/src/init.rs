// SPDX-License-Identifier: Apache-2.0
//! Starting programs from the system image (TASK-030): conductor as PID 1 and,
//! in test builds, the user test program.
//!
//! The image was verified in full by the bootloader; the program's own bytes
//! are checked again against their TOC hash (`fsi::recheck_file`) before
//! they're loaded. Each `PT_LOAD` segment gets its own VMO, mapped at its
//! linked address with its own permissions (never W and X together); the stack
//! is a VMO under `USER_STACK_TOP` with an unmapped guard page below it.

use crate::mm::vmo::Vmo;
use crate::obj::handles::Handle;
use crate::obj::{Object, ObjectRef, Resource, ResourceKind};
use crate::proc;
use crate::sched::{self, ThreadRef};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, Ordering};
use fcore_abi::layout::{DIRECT_MAP, USER_IMAGE_BASE, USER_STACK, USER_STACK_TOP};
use fcore_abi::{grant, perm, rights, well_known};

/// Conductor's pid, once started (C0 is stamped when its first thread enters user mode).
static CONDUCTOR_PID: AtomicU32 = AtomicU32::new(0);

pub fn is_conductor(pid: u32) -> bool {
    pid != 0 && CONDUCTOR_PID.load(Ordering::Relaxed) == pid
}

/// The verified system image, through the direct map.
pub fn image() -> Option<&'static [u8]> {
    let bi = crate::boot_info()?;
    if bi.image.len == 0 {
        return None;
    }
    // SAFETY: the bootloader loaded the image into memory the kernel keeps (MemKind::Image).
    Some(unsafe { core::slice::from_raw_parts((bi.image.base + DIRECT_MAP) as *const u8, bi.image.len as usize) })
}

/// A program loaded into a new process, its first thread created but not started.
pub struct Loaded {
    pub process: ObjectRef,
    pub thread: ThreadRef,
    pub entry: u64,
}

/// Loads `path` from the image into a new process called `name`.
pub fn load(path: &str, name: &str, grants: u32) -> Result<Loaded, &'static str> {
    let image = image().ok_or("the system image isn't in memory")?;
    let (_, bytes) = fsi::recheck_file(image, path).map_err(|e| e.describe())?;
    let elf = felf::Elf::parse(bytes, USER_IMAGE_BASE, USER_STACK_TOP - USER_STACK - 4096).map_err(|e| e.describe())?;
    let process = proc::create(name, grants).map_err(|e| e.describe())?;
    let aspace = proc::aspace_of(&process).ok_or("a process without an address space")?;
    for s in elf.segments() {
        let (start, end) = s.pages();
        let vmo = Vmo::new(end - start, false).map_err(|e| e.describe())?;
        vmo.write(s.vaddr - start, elf.data(s)).map_err(|e| e.describe())?;
        let mut perms = perm::READ;
        if s.writable() {
            perms |= perm::WRITE;
        }
        if s.executable() {
            perms |= perm::EXEC;
        }
        if aspace.map(&vmo, 0, end - start, start, perms as u32).map_err(|e| e.describe())? != start {
            return Err("a program segment couldn't go at its linked address");
        }
    }
    let stack = Vmo::new(USER_STACK, false).map_err(|e| e.describe())?;
    let base = USER_STACK_TOP - USER_STACK;
    if aspace.map(&stack, 0, USER_STACK, base, (perm::READ | perm::WRITE) as u32).map_err(|e| e.describe())? != base {
        return Err("the stack couldn't go at its place");
    }
    let thread = sched::create(name, sched::Class::Normal, crate::syscall::user_thread_entry, 0, true)?;
    *thread.process.lock() = Some(process.clone());
    proc::as_process(&process).map_err(|e| e.describe())?.inner.lock().threads.push(thread.clone());
    Ok(Loaded { process, thread, entry: elf.entry })
}

/// Starts a loaded program's first thread with `arg` in RDI.
pub fn start(l: &Loaded, arg: u64) -> Result<(), &'static str> {
    crate::syscall::start_user_thread(&l.thread, l.entry, USER_STACK_TOP, arg).map_err(|e| e.describe())
}

/// Conductor, PID 1: every grant, the root resource, the image, BootInfo, and
/// the event the kernel signals when it files a fault report.
pub fn start_conductor() -> Result<(), &'static str> {
    let bi = crate::boot_info().ok_or("no BootInfo")?;
    let l = load(
        "/system/conductor",
        "conductor",
        grant::PROCESS_CREATE | grant::REALTIME | grant::DMA | grant::FAULTS | grant::KERNEL_LOG | grant::BOOT_CONTROL,
    )?;
    let p = proc::as_process(&l.process).map_err(|e| e.describe())?;
    // A copy of BootInfo at offset 0 of its own page (its pointers are the
    // kernel's and mean nothing in user space; its scalars are what conductor reads).
    let bi_vmo = Vmo::new(4096, false).map_err(|e| e.describe())?;
    // SAFETY: BootInfo is plain data; this reads its bytes.
    let bi_bytes =
        unsafe { core::slice::from_raw_parts(bi as *const _ as *const u8, size_of::<fcore_abi::BootInfo>()) };
    bi_vmo.write(0, bi_bytes).map_err(|e| e.describe())?;
    let ro = rights::READ | rights::MAP | rights::DUPLICATE | rights::TRANSFER;
    {
        let mut inner = p.inner.lock();
        let root = Resource { kind: ResourceKind::Root, base: 0, len: u64::MAX };
        let add = |inner: &mut crate::proc::Inner, at: u32, object: Object, rights: u32| {
            inner.handles.add_at(at, Handle { object: Arc::new(object), rights }).map_err(|e| e.describe())
        };
        add(&mut inner, well_known::ROOT_RESOURCE, Object::Resource(root), rights::MANAGE | rights::DUPLICATE)?;
        add(&mut inner, well_known::IMAGE_VMO, Object::Vmo(Vmo::wrap(bi.image.base, bi.image.len)), ro)?;
        add(&mut inner, well_known::BOOTINFO_VMO, Object::Vmo(bi_vmo), ro)?;
        inner
            .handles
            .add_at(
                well_known::FAULTS_EVENT,
                Handle { object: crate::fault::event(), rights: rights::WAIT | rights::SIGNAL },
            )
            .map_err(|e| e.describe())?;
    }
    CONDUCTOR_PID.store(p.pid, Ordering::Relaxed);
    start(&l, 0)
}

/// Runs the user test program in `mode` and returns its exit code (ktest builds).
#[cfg(feature = "ktest")]
pub fn run_test_program(mode: u64) -> Result<i64, &'static str> {
    run_test_program_with(mode, alloc::vec::Vec::new())
}

/// As `run_test_program`, with `handles` placed in the program's table first.
#[cfg(feature = "ktest")]
pub fn run_test_program_with(mode: u64, handles: alloc::vec::Vec<(u32, Handle)>) -> Result<i64, &'static str> {
    let l = load("/system/test/utest", "utest", 0)?;
    {
        let p = proc::as_process(&l.process).map_err(|e| e.describe())?;
        let mut inner = p.inner.lock();
        for (at, h) in handles {
            inner.handles.add_at(at, h).map_err(|e| e.describe())?;
        }
    }
    start(&l, mode)?;
    let header: &crate::obj::Header = crate::obj::waitable(&l.process).map_err(|e| e.describe())?;
    let deadline = crate::arch::cpu::rdtsc() + crate::time::ns_to_ticks(5_000_000_000);
    crate::ipc::wait_one(header, fcore_abi::signal::TERMINATED, deadline)
        .map_err(|_| "the test program didn't finish in 5 s")?;
    let code = proc::as_process(&l.process).map_err(|e| e.describe())?.inner.lock().exit_code;
    code.ok_or("the test program ended without an exit code")
}

ktest!(user_a_test_program_runs_and_exits_with_its_code, {
    // Mode 0: the heap, fwire, the clock and a channel, then exit 42.
    match crate::init::run_test_program(0)? {
        42 => Ok(()),
        _ => Err("the test program's checks failed (exit code isn't 42)"),
    }
});

ktest!(user_a_null_dereference_files_a_report_with_rip_and_frames, {
    let before = crate::fault::latest_seq();
    let code = crate::init::run_test_program(1)?;
    let r = crate::fault::latest().ok_or("no fault report")?;
    if r.seq == before || code != -11 {
        return Err("a null dereference didn't end the process with a report");
    }
    if r.kind != fwire::proto::conductor::FaultKind::PageFault || r.addr != 0 || r.service != "utest" {
        return Err("the report has the wrong kind, address or service");
    }
    if !(USER_IMAGE_BASE..USER_STACK_TOP).contains(&r.rip) || r.frames.len() < 2 {
        return Err("the report lacks the faulting RIP or its frames");
    }
    Ok(())
});

ktest!(user_a_granted_port_reads_and_an_ungranted_one_faults, {
    use crate::obj::{Resource, ResourceKind};
    let com1 = Resource { kind: ResourceKind::IoPort, base: 0x3F8, len: 8 };
    let h = Handle { object: Arc::new(Object::Resource(com1)), rights: rights::MANAGE };
    // Mode 3: grant COM1 through handle 4, read its line status, exit 7.
    if crate::init::run_test_program_with(3, alloc::vec![(well_known::ROOT_RESOURCE, h)])? != 7 {
        return Err("a granted port couldn't be read");
    }
    // Mode 4: the same read with no grant is a general-protection fault.
    let before = crate::fault::latest_seq();
    crate::init::run_test_program(4)?;
    let r = crate::fault::latest().ok_or("no fault report")?;
    if r.seq == before || r.kind != fwire::proto::conductor::FaultKind::GeneralProtection {
        return Err("an ungranted port didn't fault");
    }
    crate::hw::io_switch(0, &[]);
    Ok(())
});

ktest!(user_a_panic_files_a_report_with_its_message, {
    let before = crate::fault::latest_seq();
    crate::init::run_test_program(2)?;
    let r = crate::fault::latest().ok_or("no fault report")?;
    if r.seq == before || r.kind != fwire::proto::conductor::FaultKind::Panic {
        return Err("a panic didn't file a Panic report");
    }
    if !r.message.starts_with("utest: deliberate panic") {
        return Err("the panic report doesn't carry the panic's message");
    }
    Ok(())
});
