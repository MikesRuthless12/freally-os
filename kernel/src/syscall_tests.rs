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
        // A mapping can't do more than the VMO's handle allows (closeout
        // review): MAP alone maps nothing readable or runnable; READ lets it
        // be read or run, as the image's handles are.
        let (map_only, _) = call(nr::HANDLE_DUPLICATE, vmo, rights::MAP as u64, 0, 0, 0, 0)?;
        for want in [perm::READ, perm::READ | perm::EXEC] {
            if e(call(nr::VM_MAP, well_known::SELF_ASPACE as u64, map_only, 0, 4096, 0, want))
                != Some(FErr::AccessDenied)
            {
                return Err("vm_map mapped a VMO its handle may only MAP, readable or runnable");
            }
        }
        let (readable, _) = call(nr::HANDLE_DUPLICATE, vmo, (rights::MAP | rights::READ) as u64, 0, 0, 0, 0)?;
        let (at2, _) = call(nr::VM_MAP, well_known::SELF_ASPACE as u64, readable, 0, 4096, 0, perm::READ | perm::EXEC)?;
        call(nr::VM_UNMAP, well_known::SELF_ASPACE as u64, at2, 4096, 0, 0, 0)?;
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
        // process_info: the 72-byte answer, its mapped bytes (the 8 KiB above
        // at least), and a thread's CPU time growing as it runs.
        let (n, _) = call(nr::PROCESS_INFO, well_known::SELF_PROCESS as u64, BUF + 1024, 72, 0, 0, 0)?;
        let mut info = [0u8; 72];
        crate::mm::user::copy_from_user(&mut info, BUF + 1024)?;
        let mapped = u64::from_le_bytes(info[64..72].try_into().unwrap_or_default());
        if n != 72 || mapped < 8192 || mapped != aspace.mapped_bytes() {
            return Err("process_info: 72 bytes, the bytes mapped last");
        }
        // A switch first, so the clock's started on this thread, then run.
        crate::sched::schedule();
        let before = me.run_tsc.load(core::sync::atomic::Ordering::Relaxed);
        let spin = crate::time::now_ns() + 2_000_000;
        while crate::time::now_ns() < spin {
            core::hint::spin_loop();
        }
        crate::sched::schedule();
        if me.run_tsc.load(core::sync::atomic::Ordering::Relaxed) <= before {
            return Err("a thread's CPU time didn't grow as it ran");
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
        let (len, _) = call(nr::SYSTEM_INFO, fcore_abi::topic::MEMORY, BUF, 16, 0, 0, 0)?;
        let mut m = [0u8; 16];
        crate::mm::user::copy_from_user(&mut m, BUF)?;
        let (free, total) = (
            u64::from_le_bytes(m[..8].try_into().unwrap_or_default()),
            u64::from_le_bytes(m[8..].try_into().unwrap_or_default()),
        );
        if len != 16 || free == 0 || free > total {
            return Err("system_info MEMORY: free, then the total");
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
        // A range past the 65,536 ports, or one that wraps, is refused as
        // such (closeout review: it wrapped silently, and a 65,536-port grant
        // shrank to none).
        for (base, len) in [(u64::MAX - 1, 3), (0, 0x1_0000 + 1), (0xFFFF, 2)] {
            if e(call(nr::IOPORT_GRANT, root, base, len, 0, 0, 0)) != Some(FErr::InvalidArgs) {
                return Err("ioport_grant took a range past the last port");
            }
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

ktest!(uefi_set_var_writes_freallybootctl_and_freallyclock_and_nothing_else, {
    use fcore_abi::grant;
    let p = proc::create("bootctl-test", grant::BOOT_CONTROL)?;
    let plain = proc::create("plain-test", 0)?;
    let me = crate::sched::current().ok_or("no current thread")?;
    let run = |p: &crate::obj::ObjectRef, f: &dyn Fn() -> Result<(), &'static str>| {
        *me.process.lock() = Some(p.clone());
        proc::aspace_of(p).ok_or("no address space")?.activate();
        let r = (|| {
            let (vmo, _) = call(nr::VMO_CREATE, 4096, 0, 0, 0, 0, 0)?;
            call(nr::VM_MAP, well_known::SELF_ASPACE as u64, vmo, 0, 4096, BUF, perm::READ | perm::WRITE)?;
            f()
        })();
        crate::mm::aspace::activate_kernel();
        *me.process.lock() = None;
        r
    };
    let name = fbootctl::NAME.as_bytes();
    let record = fbootctl::BootCtl::fresh("ktest").encode();
    let put = |name: &[u8], data: &[u8]| -> Result<(), &'static str> {
        crate::mm::user::copy_to_user(BUF, name)?;
        crate::mm::user::copy_to_user(BUF + 64, data)?;
        Ok(())
    };
    // Without the grant: refused.
    run(&plain, &|| {
        put(name, &record)?;
        match e(call(nr::UEFI_SET_VAR, BUF, name.len() as u64, BUF + 64, 32, 0, 0)) {
            Some(FErr::AccessDenied) => Ok(()),
            _ => Err("uefi_set_var without the boot-control grant"),
        }
    })?;
    run(&p, &|| {
        // Another variable, a wrong length, a damaged record: refused.
        put(b"BootOrder", &record)?;
        if e(call(nr::UEFI_SET_VAR, BUF, 9, BUF + 64, 32, 0, 0)) != Some(FErr::AccessDenied) {
            return Err("uefi_set_var wrote a variable other than FreallyBootCtl");
        }
        put(name, &record)?;
        if e(call(nr::UEFI_SET_VAR, BUF, name.len() as u64, BUF + 64, 31, 0, 0)) != Some(FErr::InvalidArgs) {
            return Err("uefi_set_var took a record of the wrong size");
        }
        let mut bad = record;
        bad[9] ^= 1;
        put(name, &bad)?;
        if e(call(nr::UEFI_SET_VAR, BUF, name.len() as u64, BUF + 64, 32, 0, 0)) != Some(FErr::InvalidArgs) {
            return Err("uefi_set_var took a damaged record");
        }
        // The real thing, through the firmware's runtime SetVariable.
        put(name, &record)?;
        call(nr::UEFI_SET_VAR, BUF, name.len() as u64, BUF + 64, 32, 0, 0)
            .map(|_| ())
            .map_err(|_| "the firmware's SetVariable failed")?;
        // FreallyClock (TASK-180A): 12 bytes, refused damaged or at another
        // size; taken, its zone is the kernel's at once.
        let clock = fbootctl::clock::NAME.as_bytes();
        let central = fbootctl::clock::Zone { offset_min: -360, rtc_local: true };
        let was = crate::time::zone();
        put(clock, &central.encode())?;
        if e(call(nr::UEFI_SET_VAR, BUF, clock.len() as u64, BUF + 64, 32, 0, 0)) != Some(FErr::InvalidArgs) {
            return Err("uefi_set_var took FreallyClock at the boot record's size");
        }
        let mut bad = central.encode();
        bad[4] ^= 1;
        put(clock, &bad)?;
        if e(call(nr::UEFI_SET_VAR, BUF, clock.len() as u64, BUF + 64, 12, 0, 0)) != Some(FErr::InvalidArgs) {
            return Err("uefi_set_var took a damaged FreallyClock");
        }
        if crate::time::zone() != was {
            return Err("a refused FreallyClock changed the zone");
        }
        put(clock, &central.encode())?;
        let r = call(nr::UEFI_SET_VAR, BUF, clock.len() as u64, BUF + 64, 12, 0, 0);
        let taken = crate::time::zone() == central;
        // Put back what the firmware kept before.
        put(clock, &was.encode())?;
        let back = call(nr::UEFI_SET_VAR, BUF, clock.len() as u64, BUF + 64, 12, 0, 0);
        r.map_err(|_| "the firmware's SetVariable failed for FreallyClock")?;
        back.map_err(|_| "FreallyClock couldn't be put back")?;
        if !taken {
            return Err("FreallyClock was written but the kernel didn't take its zone");
        }
        Ok(())
    })?;
    proc::terminate(&p, 0);
    proc::terminate(&plain, 0);
    Ok(())
});
