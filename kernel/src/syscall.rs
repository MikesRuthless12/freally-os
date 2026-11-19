// SPDX-License-Identifier: Apache-2.0
//! System calls (TASK-027): `SYSCALL`/`SYSRET`, the dispatch table, `FErr`
//! returns. The number is in RAX; arguments in RDI, RSI, RDX, R10, R8, R9; the
//! result in RAX (≥ 0, or a negative `FErr`) and a second value in RDX.
//!
//! - Interrupts are on while a call runs (the CPU masks them on entry; the
//!   entry stub turns them back on once it's on the thread's kernel stack).
//! - A return address that isn't canonical is never `sysret`'d (that would fault
//!   in ring 0 on Intel CPUs): the process is ended instead.
//! - `debug_log` (0) exists only in the dev flavour: compiled out, not switched off.

use crate::arch::{cpu, gdt};
use crate::ipc::{self, Message};
use crate::mm::user::{copy_from_user, copy_to_user, read_user, write_user};
use crate::mm::vmo::Vmo;
use crate::obj::handles::Handle;
use crate::obj::{Header, Object, ObjectRef, signal, waitable};
use crate::proc;
use alloc::sync::Arc;
use alloc::vec::Vec;
use fcore_abi::{FErr, clock, grant, rights, syscall as nr, topic};

/// What the entry stub saved; `dispatch` writes the results into `rax`/`rdx`.
#[repr(C)]
pub struct Frame {
    pub rax: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub r10: u64,
    pub r8: u64,
    pub r9: u64,
    pub rip: u64,
    pub rflags: u64,
    pub rsp: u64,
}

/// The stack this CPU's syscall entry switches to (`gs:[0]`, the CPU's block,
/// TASK-036): the running thread's. The scheduler sets it, interrupts off.
pub fn set_kernel_rsp(top: u64) {
    crate::smp::this().kernel_rsp.store(top, core::sync::atomic::Ordering::Relaxed);
}

core::arch::global_asm!(
    ".global syscall_entry",
    "syscall_entry:",
    "    swapgs",
    "    mov gs:[8], rsp",
    "    mov rsp, gs:[0]",
    "    push qword ptr gs:[8]", // user rsp
    "    push r11",              // user rflags
    "    push rcx",              // user rip
    "    push r9",
    "    push r8",
    "    push r10",
    "    push rdx",
    "    push rsi",
    "    push rdi",
    "    push rax",
    "    mov rdi, rsp",
    "    sti",
    "    call {dispatch}",
    "    cli",
    "    pop rax",
    "    pop rdi",
    "    pop rsi",
    "    pop rdx",
    "    pop r10",
    "    pop r8",
    "    pop r9",
    "    pop rcx",
    "    pop r11",
    "    pop rsp",
    "    swapgs",
    "    sysretq",
    // enter_user(rip, rsp, arg): the first entry to ring 3 for a new thread.
    ".global enter_user",
    "enter_user:",
    "    cli",
    "    push {uds}",
    "    push rsi",
    "    push 0x202",
    "    push {ucs}",
    "    push rdi",
    "    mov rdi, rdx",
    "    xor eax, eax",
    "    xor ebx, ebx",
    "    xor ecx, ecx",
    "    xor edx, edx",
    "    xor esi, esi",
    "    xor ebp, ebp",
    "    xor r8d, r8d",
    "    xor r9d, r9d",
    "    xor r10d, r10d",
    "    xor r11d, r11d",
    "    xor r12d, r12d",
    "    xor r13d, r13d",
    "    xor r14d, r14d",
    "    xor r15d, r15d",
    "    swapgs",
    "    iretq",
    dispatch = sym dispatch,
    uds = const gdt::USER_DS as u64,
    ucs = const gdt::USER_CS as u64,
);

unsafe extern "C" {
    fn syscall_entry();
    pub fn enter_user(rip: u64, rsp: u64, arg: u64) -> !;
}

/// This CPU's SYSCALL MSRs (every CPU, as it starts; GS is `smp`'s).
pub fn init_cpu() {
    const EFER: u32 = 0xC000_0080;
    const STAR: u32 = 0xC000_0081;
    const LSTAR: u32 = 0xC000_0082;
    const FMASK: u32 = 0xC000_0084;
    // SAFETY: the MSRs every x86-64 CPU has for SYSCALL.
    unsafe {
        cpu::wrmsr(EFER, cpu::rdmsr(EFER) | 1);
        cpu::wrmsr(STAR, (0x10u64 << 48) | ((gdt::KERNEL_CS as u64) << 32));
        cpu::wrmsr(LSTAR, syscall_entry as *const () as u64);
        // Mask IF, TF, DF, AC and NT on entry.
        cpu::wrmsr(FMASK, 0x0004_7700);
    }
}

fn canonical(a: u64) -> bool {
    let top = a >> 47;
    top == 0 || top == 0x1FFFF
}

fn err(e: FErr) -> u64 {
    e as i64 as u64
}

/// The current process's handle `h` with rights `need`.
fn handle(h: u64, need: u32) -> Result<Handle, FErr> {
    let p = proc::current().ok_or(FErr::BadState)?;
    let me = proc::as_process(&p)?;
    me.inner.lock().handles.get(h as u32, need).cloned()
}

fn add_handle(object: ObjectRef, rights: u32) -> Result<u64, FErr> {
    let p = proc::current().ok_or(FErr::BadState)?;
    let me = proc::as_process(&p)?;
    me.inner.lock().handles.add(Handle { object, rights }).map(u64::from)
}

fn grants() -> u32 {
    proc::current().and_then(|p| proc::as_process(&p).ok().map(|x| x.inner.lock().grants)).unwrap_or(0)
}

fn deadline_ticks(ns: u64) -> u64 {
    if ns == u64::MAX {
        return u64::MAX;
    }
    let now = crate::time::now_ns();
    cpu::rdtsc() + crate::time::ns_to_ticks(ns.saturating_sub(now))
}

const MAX_NAME: usize = 32;

fn user_name(ptr: u64, len: u64) -> Result<Vec<u8>, FErr> {
    if len as usize > MAX_NAME {
        return Err(FErr::InvalidArgs);
    }
    let mut b = alloc::vec![0u8; len as usize];
    copy_from_user(&mut b, ptr)?;
    core::str::from_utf8(&b).map_err(|_| FErr::InvalidArgs)?;
    Ok(b)
}

extern "C" fn dispatch(f: &mut Frame) {
    let r = call(f.rax, f.rdi, f.rsi, f.rdx, f.r10, f.r8, f.r9);
    match r {
        Ok((a, b)) => {
            f.rax = a;
            f.rdx = b;
        }
        Err(e) => f.rax = err(e),
    }
    if !canonical(f.rip)
        && let Some(p) = proc::current()
    {
        proc::terminate(&p, -1);
    }
    // On the way back to ring 3 a killed thread ends: only its registers are
    // on the stack here (everything the syscall held has been dropped).
    if crate::sched::dying() {
        crate::sched::exit_current();
    }
}

/// `debug_log` (dev only): a line straight to the kernel's log, marked
/// `debug_log`. The kernel is stripped, so `xtask validate` proves the
/// release kernel lacks this path by that text (and the dev kernel has it).
#[cfg(feature = "flavour-dev")]
#[inline(never)]
fn debug_log(ptr: u64, len: u64) -> Result<(), FErr> {
    let mut b = alloc::vec![0u8; len.min(512) as usize];
    copy_from_user(&mut b, ptr)?;
    let pid = proc::current().and_then(|p| proc::as_process(&p).ok().map(|x| x.pid)).unwrap_or(0);
    klog!("debug_log [pid {}] {}", pid, core::str::from_utf8(&b).unwrap_or("(not UTF-8)"));
    Ok(())
}

/// The table itself. `Ok((rax, rdx))`.
#[allow(clippy::too_many_arguments)]
pub fn call(n: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> Result<(u64, u64), FErr> {
    let one = |v: u64| Ok((v, 0));
    match n {
        #[cfg(feature = "flavour-dev")]
        nr::DEBUG_LOG => debug_log(a0, a1).map(|_| (0, 0)),
        nr::PROCESS_CREATE => {
            if grants() & grant::PROCESS_CREATE == 0 {
                return Err(FErr::AccessDenied);
            }
            // The child's grants: never one its creator doesn't hold.
            let child = a2 as u32;
            if a2 > u32::MAX as u64 || child & !grants() != 0 {
                return Err(FErr::AccessDenied);
            }
            let name = user_name(a0, a1)?;
            let p = proc::create(core::str::from_utf8(&name).unwrap_or("?"), child)?;
            let aspace = proc::as_process(&p)?.inner.lock().aspace.clone();
            let ph =
                add_handle(p, rights::MANAGE | rights::READ | rights::WAIT | rights::DUPLICATE | rights::TRANSFER)?;
            let ah = add_handle(Arc::new(Object::AddressSpace(aspace)), rights::MAP | rights::READ | rights::WRITE)?;
            Ok((ph, ah))
        }
        nr::PROCESS_START => {
            let p = handle(a0, rights::MANAGE)?;
            let t = handle(a1, rights::MANAGE)?;
            let arg = if a4 != 0 { Some(handle(a4, rights::TRANSFER)?) } else { None };
            let thread = match &*t.object {
                Object::Thread(t) => t.clone(),
                _ => return Err(FErr::WrongType),
            };
            let target = proc::as_process(&p.object)?;
            if let Some(arg) = arg {
                let me = proc::current().ok_or(FErr::BadState)?;
                proc::as_process(&me)?.inner.lock().handles.remove(a4 as u32)?;
                target.inner.lock().handles.add_at(fcore_abi::well_known::BOOTSTRAP, arg)?;
            }
            // The bootstrap channel is handle 1 in the child; RDI says so.
            let bootstrap = if a4 != 0 { fcore_abi::well_known::BOOTSTRAP as u64 } else { 0 };
            start_user_thread(&thread, a2, a3, bootstrap)?;
            one(0)
        }
        // The thread ends on its way back to user mode (`dispatch`).
        nr::PROCESS_EXIT => {
            if let Some(p) = proc::current() {
                proc::terminate(&p, a0 as i64);
            }
            crate::sched::kill_current();
            one(0)
        }
        nr::PROCESS_KILL => {
            let p = handle(a0, rights::MANAGE)?;
            proc::as_process(&p.object)?;
            proc::terminate(&p.object, -9);
            one(0)
        }
        nr::PROCESS_INFO => {
            let p = handle(a0, rights::READ)?;
            let pr = proc::as_process(&p.object)?;
            let inner = pr.inner.lock();
            // pid u32, threads u32, handles u32, exited u32 (1/0), exit code i64,
            // name [u8; 32], then the CPU time its live threads have run (ns, u64)
            // and the bytes mapped in its address space (u64).
            let mut out = [0u8; 72];
            out[0..4].copy_from_slice(&pr.pid.to_le_bytes());
            out[4..8].copy_from_slice(&(inner.threads.len() as u32).to_le_bytes());
            out[8..12].copy_from_slice(&(inner.handles.len() as u32).to_le_bytes());
            out[12..16].copy_from_slice(&(inner.exit_code.is_some() as u32).to_le_bytes());
            out[16..24].copy_from_slice(&inner.exit_code.unwrap_or(0).to_le_bytes());
            out[24..56].copy_from_slice(&pr.name);
            let ticks: u64 = inner.threads.iter().map(|t| t.run_tsc.load(core::sync::atomic::Ordering::Relaxed)).sum();
            out[56..64].copy_from_slice(&crate::time::ticks_to_ns(ticks).to_le_bytes());
            out[64..72].copy_from_slice(&inner.aspace.mapped_bytes().to_le_bytes());
            let n = (a2 as usize).min(out.len());
            drop(inner);
            copy_to_user(a1, &out[..n])?;
            one(n as u64)
        }
        nr::THREAD_CREATE => {
            let p = handle(a0, rights::MANAGE)?;
            let pr = proc::as_process(&p.object)?;
            let name = user_name(a1, a2)?;
            let t = crate::sched::create(
                core::str::from_utf8(&name).unwrap_or("?"),
                crate::sched::Class::Normal,
                user_thread_entry,
                0,
                true,
            )
            .map_err(|_| FErr::NoMemory)?;
            *t.process.lock() = Some(p.object.clone());
            pr.inner.lock().threads.push(t.clone());
            one(add_handle(Arc::new(Object::Thread(t)), rights::MANAGE | rights::WAIT | rights::READ)?)
        }
        nr::THREAD_START => {
            let t = handle(a0, rights::MANAGE)?;
            let thread = match &*t.object {
                Object::Thread(t) => t.clone(),
                _ => return Err(FErr::WrongType),
            };
            start_user_thread(&thread, a1, a2, a3)?;
            one(0)
        }
        nr::THREAD_EXIT => {
            crate::sched::kill_current();
            one(0)
        }
        nr::THREAD_SET_CLASS => {
            let t = handle(a0, rights::MANAGE)?;
            let class = crate::sched::Class::from_u64(a1).ok_or(FErr::InvalidArgs)?;
            if class == crate::sched::Class::Realtime && grants() & grant::REALTIME == 0 {
                return Err(FErr::AccessDenied);
            }
            match &*t.object {
                Object::Thread(t) => t.set_class(class),
                _ => return Err(FErr::WrongType),
            }
            one(0)
        }
        nr::VMO_CREATE => {
            let contiguous = a1 & 1 != 0;
            if contiguous && grants() & grant::DMA == 0 {
                return Err(FErr::AccessDenied);
            }
            let v = Vmo::new(a0, contiguous)?;
            one(add_handle(
                Arc::new(Object::Vmo(v)),
                rights::READ | rights::WRITE | rights::MAP | rights::DUPLICATE | rights::TRANSFER,
            )?)
        }
        nr::VMO_READ | nr::VMO_WRITE => {
            let reading = n == nr::VMO_READ;
            let h = handle(a0, if reading { rights::READ } else { rights::WRITE })?;
            let Object::Vmo(v) = &*h.object else { return Err(FErr::WrongType) };
            if a3 > 1 << 24 {
                return Err(FErr::InvalidArgs);
            }
            let mut buf = alloc::vec![0u8; a3 as usize];
            if reading {
                v.read(a1, &mut buf)?;
                copy_to_user(a2, &buf)?;
            } else {
                copy_from_user(&mut buf, a2)?;
                v.write(a1, &buf)?;
            }
            one(a3)
        }
        nr::VMO_PHYS => {
            if grants() & grant::DMA == 0 {
                return Err(FErr::AccessDenied);
            }
            let h = handle(a0, rights::READ)?;
            let Object::Vmo(v) = &*h.object else { return Err(FErr::WrongType) };
            one(v.page(a1).ok_or(FErr::InvalidArgs)? + a1 % 4096)
        }
        nr::VM_MAP => {
            let ah = handle(a0, rights::MAP)?;
            let vh = handle(a1, rights::MAP)?;
            let Object::AddressSpace(a) = &*ah.object else { return Err(FErr::WrongType) };
            let Object::Vmo(v) = &*vh.object else { return Err(FErr::WrongType) };
            // A mapping does no more than the VMO's handle allows: readable
            // needs READ, writable WRITE, executable EXECUTE or READ.
            let want = |p: u64| a5 & p != 0;
            let has = |r: u32| vh.rights & r != 0;
            if (want(fcore_abi::perm::READ) && !has(rights::READ))
                || (want(fcore_abi::perm::WRITE) && !has(rights::WRITE))
                || (want(fcore_abi::perm::EXEC) && !has(rights::EXECUTE | rights::READ))
            {
                return Err(FErr::AccessDenied);
            }
            one(a.map(v, a2, a3, a4, a5 as u32)?)
        }
        nr::VM_UNMAP => {
            let ah = handle(a0, rights::MAP)?;
            let Object::AddressSpace(a) = &*ah.object else { return Err(FErr::WrongType) };
            a.unmap(a1, a2)?;
            one(0)
        }
        nr::VM_PROTECT => {
            let ah = handle(a0, rights::MAP)?;
            let Object::AddressSpace(a) = &*ah.object else { return Err(FErr::WrongType) };
            a.protect(a1, a2, a3 as u32)?;
            one(0)
        }
        nr::CHANNEL_CREATE => {
            let (x, y) = ipc::channel_pair();
            let r = rights::READ | rights::WRITE | rights::WAIT | rights::TRANSFER | rights::DUPLICATE;
            let h0 = add_handle(Arc::new(Object::Channel(x)), r)?;
            let h1 = add_handle(Arc::new(Object::Channel(y)), r)?;
            Ok((h0, h1))
        }
        nr::CHANNEL_WRITE => {
            let h = handle(a0, rights::WRITE)?;
            let Object::Channel(end) = &*h.object else { return Err(FErr::WrongType) };
            let msg = take_message(a1, a2, a3, a4)?;
            ipc::write(end, msg)?;
            one(0)
        }
        nr::CHANNEL_READ => {
            let h = handle(a0, rights::READ)?;
            let Object::Channel(end) = &*h.object else { return Err(FErr::WrongType) };
            match ipc::read(end, a2 as usize, a4 as usize) {
                Ok(m) => give_message(m, a1, a3, a5),
                Err((e, nb, nh)) => {
                    if e == FErr::BufferTooSmall && a5 != 0 {
                        let _ = write_user(a5, &[nb as u32, nh as u32]);
                    }
                    Err(e)
                }
            }
        }
        nr::CHANNEL_CALL => {
            // a1: the request (ptr to [bytes, len, handles, nhandles]); a2: the reply
            // buffer (ptr to [bytes, cap, handles, hcap, actual]); a3: deadline (ns).
            let h = handle(a0, rights::READ | rights::WRITE)?;
            let Object::Channel(end) = &*h.object else { return Err(FErr::WrongType) };
            let req: [u64; 4] = read_user(a1)?;
            let rep: [u64; 5] = read_user(a2)?;
            let msg = take_message(req[0], req[1], req[2], req[3])?;
            match ipc::call(end, msg, rep[1] as usize, rep[3] as usize, deadline_ticks(a3)) {
                Ok(m) => give_message(m, rep[0], rep[2], rep[4]),
                Err((e, ..)) => Err(e),
            }
        }
        nr::OBJECT_WAIT_ONE => {
            let h = handle(a0, rights::WAIT)?;
            let hd = waitable(&h.object)?;
            one(ipc::wait_one(hd, a1 as u32, deadline_ticks(a2))? as u64)
        }
        nr::OBJECT_WAIT_MANY => {
            if a1 > 64 {
                return Err(FErr::InvalidArgs);
            }
            let mut objs = Vec::new();
            for i in 0..a1 {
                let item: [u32; 2] = read_user(a0 + i * 8)?; // (handle, signals)
                objs.push((handle(item[0] as u64, rights::WAIT)?, item[1]));
            }
            let items: Result<Vec<(&Header, u32)>, FErr> =
                objs.iter().map(|(h, s)| Ok((waitable(&h.object)?, *s))).collect();
            one(ipc::wait_many(&items?, deadline_ticks(a2))? as u64)
        }
        nr::OBJECT_SIGNAL => {
            let h = handle(a0, rights::SIGNAL)?;
            if (a1 | a2) as u32 & !signal::USER_MASK != 0 {
                return Err(FErr::InvalidArgs);
            }
            waitable(&h.object)?.update(a1 as u32, a2 as u32);
            one(0)
        }
        nr::EVENT_CREATE => one(add_handle(
            Arc::new(Object::Event(Header::new())),
            rights::WAIT | rights::SIGNAL | rights::DUPLICATE | rights::TRANSFER,
        )?),
        nr::HANDLE_CLOSE => {
            let p = proc::current().ok_or(FErr::BadState)?;
            proc::as_process(&p)?.inner.lock().handles.remove(a0 as u32)?;
            one(0)
        }
        nr::HANDLE_DUPLICATE => {
            let p = proc::current().ok_or(FErr::BadState)?;
            one(proc::as_process(&p)?.inner.lock().handles.duplicate(a0 as u32, a1 as u32)? as u64)
        }
        // FreallyBootCtl and nothing else: conductor (the boot-control grant)
        // writes the A/B record; the kernel refuses any malformed one.
        nr::UEFI_SET_VAR => {
            if grants() & grant::BOOT_CONTROL == 0 {
                return Err(FErr::AccessDenied);
            }
            let mut name = [0u8; 32];
            if a1 as usize > name.len() {
                return Err(FErr::AccessDenied);
            }
            copy_from_user(&mut name[..a1 as usize], a0)?;
            if &name[..a1 as usize] != fbootctl::NAME.as_bytes() {
                return Err(FErr::AccessDenied);
            }
            let mut data = [0u8; fbootctl::SIZE];
            if a3 as usize != data.len() {
                return Err(FErr::InvalidArgs);
            }
            copy_from_user(&mut data, a2)?;
            if fbootctl::BootCtl::decode(&data).is_none() {
                return Err(FErr::InvalidArgs);
            }
            crate::uefi::set_boot_ctl(&data).map(|_| (0, 0))
        }
        nr::RESOURCE_CREATE => {
            let parent = crate::hw::resource_of(&handle(a0, rights::MANAGE)?.object)?;
            let kind = match a1 {
                fcore_abi::resource::MMIO => crate::obj::ResourceKind::Mmio,
                fcore_abi::resource::IO_PORT => crate::obj::ResourceKind::IoPort,
                fcore_abi::resource::IRQ => crate::obj::ResourceKind::Irq,
                _ => return Err(FErr::InvalidArgs),
            };
            let child = crate::hw::create(&parent, kind, a2, a3)?;
            one(add_handle(Arc::new(Object::Resource(child)), rights::MANAGE | rights::DUPLICATE | rights::TRANSFER)?)
        }
        nr::IOPORT_GRANT => {
            let r = crate::hw::resource_of(&handle(a0, rights::MANAGE)?.object)?;
            // Inside the 65,536 ports, and a count a u16 holds (no wrap, no truncation).
            if a2 == 0 || a2 > 0xFFFF || a1.checked_add(a2).is_none_or(|end| end > 0x1_0000) {
                return Err(FErr::InvalidArgs);
            }
            if !crate::hw::covers(&r, crate::obj::ResourceKind::IoPort, a1, a2) {
                return Err(FErr::AccessDenied);
            }
            let p = proc::current().ok_or(FErr::BadState)?;
            let me = proc::as_process(&p)?;
            crate::hw::grant_ports(me.pid, &me.io, a1 as u16, a2 as u16);
            one(0)
        }
        nr::MMIO_MAP => {
            let r = crate::hw::resource_of(&handle(a0, rights::MANAGE)?.object)?;
            let (phys, len) = (a1, a2);
            if len == 0 || !phys.is_multiple_of(4096) {
                return Err(FErr::InvalidArgs);
            }
            if !crate::hw::covers(&r, crate::obj::ResourceKind::Mmio, phys, len) {
                return Err(FErr::AccessDenied);
            }
            if crate::hw::is_kernel_memory(phys, len) {
                return Err(FErr::AccessDenied);
            }
            let mut perms = crate::mm::aspace::perm::READ;
            if a3 & fcore_abi::cache::READ_ONLY == 0 {
                perms |= crate::mm::aspace::perm::WRITE;
            }
            perms |= match a3 & 3 {
                fcore_abi::cache::WRITE_BACK => 0,
                fcore_abi::cache::WRITE_COMBINING => crate::mm::aspace::perm::WRITE_COMBINING,
                fcore_abi::cache::UNCACHED => crate::mm::aspace::perm::UNCACHED,
                _ => return Err(FErr::InvalidArgs),
            };
            let p = proc::current().ok_or(FErr::BadState)?;
            let aspace = proc::aspace_of(&p).ok_or(FErr::BadState)?;
            one(aspace.map(&Vmo::wrap(phys, len), 0, len, 0, perms)?)
        }
        // The boot-control grant (conductor): the machine resets, through the
        // chipset's reset register, else the keyboard controller, else a
        // triple fault.
        nr::SYSTEM_RESET => {
            if grants() & grant::BOOT_CONTROL == 0 {
                return Err(FErr::AccessDenied);
            }
            crate::arch::cpu::reset()
        }
        // e2e kernels only, conductor only: a panic on purpose, so a spec can
        // prove the last-panic page reaches Monitor on the next boot (TASK-074).
        #[cfg(feature = "flavour-e2e")]
        nr::TEST_PANIC => {
            let pid = proc::current().and_then(|p| proc::as_process(&p).ok().map(|x| x.pid)).unwrap_or(0);
            if !crate::init::is_conductor(pid) {
                return Err(FErr::AccessDenied);
            }
            panic!("a test panic, asked for through the e2e test door")
        }
        nr::VMO_CREATE_PHYSICAL => {
            let r = crate::hw::resource_of(&handle(a0, rights::MANAGE)?.object)?;
            let (phys, len) = (a1, a2);
            if len == 0 || !phys.is_multiple_of(4096) || len > crate::mm::vmo::MAX_SIZE {
                return Err(FErr::InvalidArgs);
            }
            if !crate::hw::covers(&r, crate::obj::ResourceKind::Mmio, phys, len)
                || crate::hw::is_kernel_memory(phys, len)
            {
                return Err(FErr::AccessDenied);
            }
            let cache = match a3 & 3 {
                fcore_abi::cache::WRITE_BACK => 0,
                fcore_abi::cache::WRITE_COMBINING => crate::mm::aspace::perm::WRITE_COMBINING,
                fcore_abi::cache::UNCACHED => crate::mm::aspace::perm::UNCACHED,
                _ => return Err(FErr::InvalidArgs),
            };
            one(add_handle(
                Arc::new(Object::Vmo(Vmo::physical(phys, len, cache))),
                rights::READ | rights::WRITE | rights::MAP | rights::DUPLICATE | rights::TRANSFER,
            )?)
        }
        nr::INTERRUPT_BIND => {
            let r = crate::hw::resource_of(&handle(a0, rights::MANAGE)?.object)?;
            if !crate::hw::covers(&r, crate::obj::ResourceKind::Irq, a1, 1) || a1 > u32::MAX as u64 {
                return Err(FErr::AccessDenied);
            }
            let line = crate::hw::bind(a1 as u32, a2 as u16)?;
            one(add_handle(Arc::new(Object::Interrupt(line)), rights::WAIT | rights::MANAGE)?)
        }
        nr::INTERRUPT_ACK => {
            let h = handle(a0, rights::MANAGE)?;
            let Object::Interrupt(l) = &*h.object else { return Err(FErr::WrongType) };
            crate::hw::ack(l);
            one(0)
        }
        nr::CLOCK_GET => one(match a0 {
            clock::MONOTONIC => crate::time::now_ns(),
            clock::BOOT => crate::time::boot_ns(),
            clock::UTC => crate::time::utc_ns(),
            _ => return Err(FErr::InvalidArgs),
        }),
        nr::FUTEX_WAIT => crate::futex::wait(a0, a1 as u32, deadline_ticks(a2)).map(|_| (0, 0)),
        nr::FUTEX_WAKE => one(crate::futex::wake(a0, a1 as u32)),
        nr::SYSTEM_INFO => one(system_info(a0, a1, a2)?),
        _ => Err(FErr::NotSupported),
    }
}

/// Copies a message in from user memory, moving its handles out of the caller's table.
fn take_message(bytes: u64, len: u64, handles: u64, nhandles: u64) -> Result<Message, FErr> {
    if len as usize > ipc::MAX_BYTES || nhandles as usize > ipc::MAX_HANDLES {
        return Err(FErr::InvalidArgs);
    }
    let mut b = alloc::vec![0u8; len as usize];
    copy_from_user(&mut b, bytes)?;
    let mut hv = alloc::vec![0u32; nhandles as usize];
    for (i, h) in hv.iter_mut().enumerate() {
        *h = read_user(handles + i as u64 * 4)?;
    }
    let p = proc::current().ok_or(FErr::BadState)?;
    let me = proc::as_process(&p)?;
    let mut inner = me.inner.lock();
    // Check every handle first, so a bad one moves none.
    for h in &hv {
        inner.handles.get(*h, rights::TRANSFER)?;
    }
    let moved: Result<Vec<Handle>, FErr> = hv.iter().map(|h| inner.handles.remove(*h)).collect();
    Ok(Message { bytes: b, handles: moved? })
}

/// Copies a message out to user memory, adding its handles to the caller's table.
fn give_message(mut m: Message, bytes: u64, handles: u64, actual: u64) -> Result<(u64, u64), FErr> {
    copy_to_user(bytes, &m.bytes)?;
    let p = proc::current().ok_or(FErr::BadState)?;
    let me = proc::as_process(&p)?;
    let mut values = Vec::new();
    {
        let mut inner = me.inner.lock();
        for h in core::mem::take(&mut m.handles) {
            values.push(inner.handles.add(h)?);
        }
    }
    for (i, v) in values.iter().enumerate() {
        write_user(handles + i as u64 * 4, v)?;
    }
    if actual != 0 {
        write_user(actual, &[m.bytes.len() as u32, values.len() as u32])?;
    }
    Ok((m.bytes.len() as u64, values.len() as u64))
}

fn system_info(t: u64, buf: u64, len: u64) -> Result<u64, FErr> {
    let mut out: Vec<u8> = Vec::new();
    match t {
        topic::CPU => {
            let cpus = crate::smp::online() as u64;
            out.extend_from_slice(&cpus.to_le_bytes());
            out.extend_from_slice(&crate::time::tsc_hz().to_le_bytes());
        }
        topic::MEMORY => {
            // Free bytes, then the bytes there were to give at boot.
            let free = crate::mm::frames::free_count() * 4096;
            out.extend_from_slice(&free.to_le_bytes());
            out.extend_from_slice(&(crate::mm::frames::total_count() * 4096).to_le_bytes());
        }
        topic::BOOT_TIMELINE => {
            for (id, tsc) in crate::timeline::stamps() {
                out.push(id);
                out.extend_from_slice(&tsc.to_le_bytes());
            }
        }
        topic::KERNEL_LOG => {
            if grants() & grant::KERNEL_LOG == 0 {
                return Err(FErr::AccessDenied);
            }
            out = crate::serial::ring_snapshot();
        }
        topic::FAULTS => {
            if grants() & grant::FAULTS == 0 {
                return Err(FErr::AccessDenied);
            }
            out = crate::fault::encode_all();
        }
        topic::CPUS => {
            let n = crate::smp::count();
            out.extend_from_slice(&[
                crate::arch::apic::x2apic() as u8,
                crate::arch::apic::tsc_deadline_supported() as u8,
                crate::smp::hybrid() as u8,
                crate::time::tsc_from(),
            ]);
            out.extend_from_slice(&(n as u32).to_le_bytes());
            for c in (0..n).filter_map(crate::smp::cpu) {
                out.extend_from_slice(&c.apic_id.load(core::sync::atomic::Ordering::Acquire).to_le_bytes());
                out.push(c.core_type.load(core::sync::atomic::Ordering::Acquire));
                out.push(c.online.load(core::sync::atomic::Ordering::Acquire) as u8);
                out.extend_from_slice(&[0, 0]);
            }
        }
        topic::EDID => {
            if let Some(bi) = crate::boot_info()
                && bi.edid.len > 0
                && bi.edid.len <= 4096
            {
                // SAFETY: the bootloader's copy of the EDID, in BOOT_DATA
                // memory the kernel keeps, at its direct-map address.
                out.extend_from_slice(unsafe { core::slice::from_raw_parts(bi.edid.ptr, bi.edid.len as usize) });
            }
        }
        topic::IMAGE => {
            if let (Some(bi), Some(img)) = (crate::boot_info(), crate::init::image())
                && img.len() >= 64
            {
                // The header the bootloader verified: flavour at 10, version at
                // 12..44, build id at 44..64 (PRD § 3, `FsiHeader`).
                out.push(bi.image.slot as u8);
                out.push(img[10]);
                out.extend_from_slice(&[0, 0]);
                out.extend_from_slice(&img[12..64]);
            }
        }
        topic::GOP_MODES => {
            if let Some(bi) = crate::boot_info() {
                // BootInfo v2 had no list.
                if bi.version >= 3 {
                    for &m in bi.gop_modes.iter().take_while(|&&m| m != 0) {
                        out.extend_from_slice(&m.to_le_bytes());
                    }
                }
                out.extend_from_slice(&0u32.to_le_bytes());
                let fb = &bi.framebuffer;
                out.extend_from_slice(&((fb.width << 16) | (fb.height & 0xFFFF)).to_le_bytes());
            }
        }
        _ => return Err(FErr::NotSupported),
    }
    let n = out.len().min(len as usize);
    copy_to_user(buf, &out[..n])?;
    Ok(out.len() as u64)
}

/// The kernel side of a user thread before its first entry to ring 3.
pub extern "C" fn user_thread_entry(start: u64) {
    // Killed before it was started (no `start`), or before it first ran: end
    // here (the thread trampoline's exit).
    if start == 0 {
        return;
    }
    // SAFETY: `start` is a Box<[u64; 3]> leaked by start_user_thread.
    let s = unsafe { alloc::boxed::Box::from_raw(start as *mut [u64; 3]) };
    let [rip, rsp, arg] = *s;
    if crate::sched::dying() {
        return;
    }
    let pid = crate::sched::current()
        .and_then(|t| t.process.lock().as_ref().and_then(|p| proc::as_process(p).ok().map(|x| x.pid)))
        .unwrap_or(0);
    if crate::init::is_conductor(pid) {
        crate::timeline::first_user_entry();
    }
    // SAFETY: the thread's address space is active (the scheduler switched to it).
    unsafe { enter_user(rip, rsp, arg) }
}

pub fn start_user_thread(t: &crate::sched::ThreadRef, rip: u64, rsp: u64, arg: u64) -> Result<(), FErr> {
    if !canonical(rip) || rip >= crate::mm::aspace::USER_TOP || rsp >= crate::mm::aspace::USER_TOP {
        return Err(FErr::InvalidArgs);
    }
    let start = alloc::boxed::Box::into_raw(alloc::boxed::Box::new([rip, rsp, arg])) as u64;
    crate::sched::set_start_arg(t, start);
    crate::sched::start(t);
    Ok(())
}
