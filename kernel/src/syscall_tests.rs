// SPDX-License-Identifier: Apache-2.0
//! The syscall table, driven from a ktest (TASK-027's verify): each call's
//! success and its error paths, through the real dispatcher, inside a real
//! process with its own address space and user memory.

use crate::proc;
use crate::syscall::call;
use fcore_abi::{FErr, clock, perm, rights, syscall as nr, well_known};

const BUF: u64 = 0x50_0000;

fn e(r: Result<(u64, u64), FErr>) -> Option<FErr> {
    r.err()
}

ktest!(syscalls_each_succeeds_and_each_error_path_is_reported, {
    let p = proc::create("syscall-test", 0)?;
    let me = crate::sched::current().ok_or("no current thread")?;
    *me.process.lock() = Some(p.clone());
    let aspace = proc::aspace_of(&p).ok_or("no address space")?;
    aspace.activate();
    let result = (|| -> Result<(), &'static str> {
        // VMOs and mappings: user memory to pass buffers through.
        let (vmo, _) = call(nr::VMO_CREATE, 8192, 0, 0, 0, 0, 0)?;
        let (at, _) = call(nr::VM_MAP, well_known::SELF_ASPACE as u64, vmo, 0, 8192, BUF, perm::READ | perm::WRITE)?;
        if at != BUF {
            return Err("vm_map didn't honour its hint");
        }
        if e(call(nr::VM_MAP, well_known::SELF_ASPACE as u64, vmo, 0, 4096, 0, perm::WRITE | perm::EXEC))
            != Some(FErr::InvalidArgs)
        {
            return Err("vm_map allowed W+X");
        }
        if e(call(nr::VMO_CREATE, 4096, 1, 0, 0, 0, 0)) != Some(FErr::AccessDenied) {
            return Err("a contiguous VMO without the DMA grant");
        }
        crate::mm::user::copy_to_user(BUF, b"hello, freally")?;
        call(nr::VMO_WRITE, vmo, 4096, BUF, 14, 0, 0)?;
        call(nr::VMO_READ, vmo, 4096, BUF + 100, 14, 0, 0)?;
        let mut back = [0u8; 14];
        crate::mm::user::copy_from_user(&mut back, BUF + 100)?;
        if &back != b"hello, freally" {
            return Err("vmo_write then vmo_read changed the bytes");
        }
        if e(call(nr::VMO_READ, vmo, 8000, BUF, 500, 0, 0)) != Some(FErr::InvalidArgs) {
            return Err("vmo_read past the end succeeded");
        }
        if e(call(nr::VMO_READ, vmo, 0, 0xFFFF_8000_0000_0000, 8, 0, 0)) != Some(FErr::InvalidArgs) {
            return Err("vmo_read wrote into the kernel");
        }
        call(nr::VM_PROTECT, well_known::SELF_ASPACE as u64, BUF, 8192, perm::READ, 0, 0)?;
        call(nr::VM_PROTECT, well_known::SELF_ASPACE as u64, BUF, 8192, perm::READ | perm::WRITE, 0, 0)?;
        // Events, signals, waits.
        let (ev, _) = call(nr::EVENT_CREATE, 0, 0, 0, 0, 0, 0)?;
        if e(call(nr::OBJECT_SIGNAL, ev, 0, 1, 0, 0, 0)) != Some(FErr::InvalidArgs) {
            return Err("object_signal set a kernel bit");
        }
        call(nr::OBJECT_SIGNAL, ev, 0, 1 << 24, 0, 0, 0)?;
        let (seen, _) = call(nr::OBJECT_WAIT_ONE, ev, 1 << 24, 0, 0, 0, 0)?;
        if seen & (1 << 24) == 0 {
            return Err("object_wait_one didn't see the signal");
        }
        let deadline = crate::time::now_ns() + 1_000_000;
        if e(call(nr::OBJECT_WAIT_ONE, ev, 1 << 25, deadline, 0, 0, 0)) != Some(FErr::TimedOut) {
            return Err("object_wait_one didn't time out");
        }
        // Handles: duplicate narrows only; close; a closed handle is bad.
        let (dup, _) = call(nr::HANDLE_DUPLICATE, ev, rights::WAIT as u64, 0, 0, 0, 0)?;
        if e(call(nr::OBJECT_SIGNAL, dup, 0, 1 << 24, 0, 0, 0)) != Some(FErr::AccessDenied) {
            return Err("a narrowed handle could signal");
        }
        if e(call(nr::HANDLE_DUPLICATE, dup, (rights::WAIT | rights::SIGNAL) as u64, 0, 0, 0, 0))
            != Some(FErr::AccessDenied)
        {
            return Err("handle_duplicate widened rights");
        }
        call(nr::HANDLE_CLOSE, dup, 0, 0, 0, 0, 0)?;
        if e(call(nr::HANDLE_CLOSE, dup, 0, 0, 0, 0, 0)) != Some(FErr::BadHandle) {
            return Err("closing a closed handle succeeded");
        }
        // Rights are checked before types: a VMO has READ, so reading it as a channel is WrongType.
        if e(call(nr::CHANNEL_READ, vmo, BUF, 1, 0, 0, 0)) != Some(FErr::WrongType) {
            return Err("a VMO was read as a channel");
        }
        // Channels through user memory.
        let (c0, c1) = call(nr::CHANNEL_CREATE, 0, 0, 0, 0, 0, 0)?;
        crate::mm::user::copy_to_user(BUF, b"ping")?;
        call(nr::CHANNEL_WRITE, c0, BUF, 4, 0, 0, 0)?;
        if e(call(nr::CHANNEL_READ, c1, BUF + 200, 2, 0, 0, BUF + 300)) != Some(FErr::BufferTooSmall) {
            return Err("a too-small channel_read didn't say so");
        }
        let (n, _) = call(nr::CHANNEL_READ, c1, BUF + 200, 64, 0, 0, 0)?;
        let mut got = [0u8; 4];
        crate::mm::user::copy_from_user(&mut got, BUF + 200)?;
        if n != 4 || &got != b"ping" {
            return Err("the channel changed the message");
        }
        call(nr::HANDLE_CLOSE, c0, 0, 0, 0, 0, 0)?;
        if e(call(nr::CHANNEL_WRITE, c1, BUF, 4, 0, 0, 0)) != Some(FErr::PeerClosed) {
            return Err("writing to a closed peer succeeded");
        }
        // Clocks, grants, system_info, unknown numbers.
        let (t, _) = call(nr::CLOCK_GET, clock::MONOTONIC, 0, 0, 0, 0, 0)?;
        if t == 0 || e(call(nr::CLOCK_GET, 9, 0, 0, 0, 0, 0)) != Some(FErr::InvalidArgs) {
            return Err("clock_get");
        }
        crate::mm::user::copy_to_user(BUF, b"child")?;
        if e(call(nr::PROCESS_CREATE, BUF, 5, 0, 0, 0, 0)) != Some(FErr::AccessDenied) {
            return Err("process_create without the grant");
        }
        let (len, _) = call(nr::SYSTEM_INFO, fcore_abi::topic::MEMORY, BUF, 8, 0, 0, 0)?;
        if len != 8 {
            return Err("system_info MEMORY");
        }
        if e(call(nr::SYSTEM_INFO, fcore_abi::topic::FAULTS, BUF, 8, 0, 0, 0)) != Some(FErr::AccessDenied) {
            return Err("system_info FAULTS without the grant");
        }
        if e(call(999, 0, 0, 0, 0, 0, 0)) != Some(FErr::NotSupported) {
            return Err("an unknown syscall number");
        }
        #[cfg(not(feature = "flavour-dev"))]
        if e(call(nr::DEBUG_LOG, BUF, 4, 0, 0, 0, 0)) != Some(FErr::NotSupported) {
            return Err("debug_log exists outside the dev flavour");
        }
        call(nr::VM_UNMAP, well_known::SELF_ASPACE as u64, BUF, 8192, 0, 0, 0)?;
        Ok(())
    })();
    crate::mm::aspace::activate_kernel();
    *me.process.lock() = None;
    proc::terminate(&p, 0);
    result
});

ktest!(hardware_access_stays_inside_each_grant, {
    use crate::obj::handles::Handle;
    use crate::obj::{Object, Resource, ResourceKind};
    use alloc::sync::Arc;
    use fcore_abi::{cache, resource};
    let p = proc::create("hw-test", 0)?;
    let me = crate::sched::current().ok_or("no current thread")?;
    *me.process.lock() = Some(p.clone());
    proc::aspace_of(&p).ok_or("no address space")?.activate();
    let root = {
        let pr = proc::as_process(&p)?;
        let r = Resource { kind: ResourceKind::Root, base: 0, len: u64::MAX };
        pr.inner.lock().handles.add(Handle { object: Arc::new(Object::Resource(r)), rights: rights::MANAGE })? as u64
    };
    let result = (|| -> Result<(), &'static str> {
        // Carving: COM1's ports from the root; nothing outside them from that.
        let (com1, _) = call(nr::RESOURCE_CREATE, root, resource::IO_PORT, 0x3F8, 8, 0, 0)?;
        if e(call(nr::RESOURCE_CREATE, com1, resource::IO_PORT, 0x60, 1, 0, 0)) != Some(FErr::AccessDenied) {
            return Err("a child resource widened its parent");
        }
        if e(call(nr::RESOURCE_CREATE, com1, resource::IRQ, 4, 1, 0, 0)) != Some(FErr::AccessDenied) {
            return Err("an I/O-port resource made an IRQ resource");
        }
        // Ports: inside the grant, yes; outside, no.
        if e(call(nr::IOPORT_GRANT, com1, 0x60, 1, 0, 0, 0)) != Some(FErr::AccessDenied) {
            return Err("ioport_grant opened a port outside its resource");
        }
        call(nr::IOPORT_GRANT, com1, 0x3F8, 8, 0, 0, 0)?;
        // MMIO: never RAM the kernel hands out; the framebuffer, yes.
        let ram = crate::mm::frames::alloc().ok_or("no frame")?;
        let refused = e(call(nr::MMIO_MAP, root, ram, 4096, cache::UNCACHED, 0, 0));
        crate::mm::frames::free(ram);
        if refused != Some(FErr::AccessDenied) {
            return Err("mmio_map mapped RAM");
        }
        let fb = crate::boot_info().ok_or("no BootInfo")?.framebuffer.base;
        let (fbres, _) = call(nr::RESOURCE_CREATE, root, resource::MMIO, fb, 4096, 0, 0)?;
        if e(call(nr::MMIO_MAP, fbres, fb + 4096, 4096, cache::WRITE_COMBINING, 0, 0)) != Some(FErr::AccessDenied) {
            return Err("mmio_map went past its resource");
        }
        let (at, _) = call(nr::MMIO_MAP, fbres, fb, 4096, cache::WRITE_COMBINING, 0, 0)?;
        let mut px = [0u8; 4];
        crate::mm::user::copy_from_user(&mut px, at)?;
        // SAFETY: the framebuffer's first pixel, through the direct map.
        let direct = unsafe { core::ptr::read_volatile((fb + fcore_abi::layout::DIRECT_MAP) as *const [u8; 4]) };
        if px != direct {
            return Err("the mapped framebuffer isn't the framebuffer");
        }
        // Interrupts: only the covered line, and only once.
        let (irq4, _) = call(nr::RESOURCE_CREATE, root, resource::IRQ, 4, 1, 0, 0)?;
        if e(call(nr::INTERRUPT_BIND, irq4, 1, 0, 0, 0, 0)) != Some(FErr::AccessDenied) {
            return Err("interrupt_bind bound a line outside its resource");
        }
        let (line, _) = call(nr::INTERRUPT_BIND, irq4, 4, 0, 0, 0, 0)?;
        if e(call(nr::INTERRUPT_BIND, irq4, 4, 0, 0, 0, 0)) != Some(FErr::AlreadyExists) {
            return Err("one line was bound twice");
        }
        call(nr::INTERRUPT_ACK, line, 0, 0, 0, 0, 0)?;
        call(nr::HANDLE_CLOSE, line, 0, 0, 0, 0, 0)?;
        Ok(())
    })();
    crate::mm::aspace::activate_kernel();
    crate::hw::io_switch(0, &[]);
    *me.process.lock() = None;
    proc::terminate(&p, 0);
    result
});
