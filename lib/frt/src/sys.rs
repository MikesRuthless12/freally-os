// SPDX-License-Identifier: Apache-2.0
//! The system calls (PRD § 4): number in RAX, arguments in RDI, RSI, RDX, R10,
//! R8, R9; the result in RAX (≥ 0, or a negative `FErr`), a second in RDX.

use fcore_abi::{FErr, syscall as nr};

#[inline(always)]
pub fn raw(n: u64, a: [u64; 6]) -> (i64, u64) {
    let (rax, rdx): (i64, u64);
    // SAFETY: the kernel's system-call ABI: it preserves everything but RCX and
    // R11 (which SYSCALL itself uses) and the result registers.
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") n as i64 => rax,
            in("rdi") a[0],
            in("rsi") a[1],
            inlateout("rdx") a[2] => rdx,
            in("r10") a[3],
            in("r8") a[4],
            in("r9") a[5],
            out("rcx") _,
            out("r11") _,
            options(nostack),
        );
    }
    (rax, rdx)
}

pub fn call(n: u64, a: [u64; 6]) -> Result<(u64, u64), FErr> {
    let (r, d) = raw(n, a);
    if r < 0 { Err(FErr::from_code(r)) } else { Ok((r as u64, d)) }
}

pub fn call1(n: u64, a: [u64; 6]) -> Result<u64, FErr> {
    call(n, a).map(|(r, _)| r)
}

/// Writes to the kernel log (the dev flavour only; elsewhere it's a no-op).
pub fn debug_log(msg: &[u8]) {
    let _ = call(nr::DEBUG_LOG, [msg.as_ptr() as u64, msg.len() as u64, 0, 0, 0, 0]);
}

pub fn process_exit(code: i64) -> ! {
    let _ = raw(nr::PROCESS_EXIT, [code as u64, 0, 0, 0, 0, 0]);
    loop {
        core::hint::spin_loop();
    }
}

pub fn vmo_create(size: u64) -> Result<u32, FErr> {
    call1(nr::VMO_CREATE, [size, 0, 0, 0, 0, 0]).map(|h| h as u32)
}

pub fn vm_map(aspace: u32, vmo: u32, off: u64, len: u64, hint: u64, perms: u64) -> Result<u64, FErr> {
    call1(nr::VM_MAP, [aspace as u64, vmo as u64, off, len, hint, perms])
}

pub fn handle_close(h: u32) -> Result<(), FErr> {
    call1(nr::HANDLE_CLOSE, [h as u64, 0, 0, 0, 0, 0]).map(|_| ())
}

pub fn clock_get(clock: u64) -> u64 {
    call1(nr::CLOCK_GET, [clock, 0, 0, 0, 0, 0]).unwrap_or(0)
}

pub fn event_create() -> Result<u32, FErr> {
    call1(nr::EVENT_CREATE, [0; 6]).map(|h| h as u32)
}

/// Waits for `signals` on `h` until `deadline_ns` (monotonic; u64::MAX forever).
pub fn wait_one(h: u32, signals: u32, deadline_ns: u64) -> Result<u32, FErr> {
    call1(nr::OBJECT_WAIT_ONE, [h as u64, signals as u64, deadline_ns, 0, 0, 0]).map(|s| s as u32)
}

pub fn channel_create() -> Result<(u32, u32), FErr> {
    call(nr::CHANNEL_CREATE, [0; 6]).map(|(a, b)| (a as u32, b as u32))
}

pub fn channel_write(h: u32, bytes: &[u8], handles: &[u32]) -> Result<(), FErr> {
    call1(
        nr::CHANNEL_WRITE,
        [h as u64, bytes.as_ptr() as u64, bytes.len() as u64, handles.as_ptr() as u64, handles.len() as u64, 0],
    )
    .map(|_| ())
}

/// Reads one message; returns (bytes, handles) received.
pub fn channel_read(h: u32, bytes: &mut [u8], handles: &mut [u32]) -> Result<(usize, usize), FErr> {
    call(
        nr::CHANNEL_READ,
        [h as u64, bytes.as_mut_ptr() as u64, bytes.len() as u64, handles.as_mut_ptr() as u64, handles.len() as u64, 0],
    )
    .map(|(a, b)| (a as usize, b as usize))
}

pub fn system_info(topic: u64, buf: &mut [u8]) -> Result<usize, FErr> {
    call1(nr::SYSTEM_INFO, [topic, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0, 0]).map(|n| n as usize)
}

/// A synchronous request: writes `req` (and moves `req_handles`), then waits
/// until `deadline_ns` for the reply. Returns (reply bytes, reply handles).
pub fn channel_call(
    h: u32,
    req: &[u8],
    req_handles: &[u32],
    reply: &mut [u8],
    reply_handles: &mut [u32],
    deadline_ns: u64,
) -> Result<(usize, usize), FErr> {
    let wr: [u64; 4] = [req.as_ptr() as u64, req.len() as u64, req_handles.as_ptr() as u64, req_handles.len() as u64];
    let mut actual = [0u32; 2];
    let rd: [u64; 5] = [
        reply.as_mut_ptr() as u64,
        reply.len() as u64,
        reply_handles.as_mut_ptr() as u64,
        reply_handles.len() as u64,
        actual.as_mut_ptr() as u64,
    ];
    call(nr::CHANNEL_CALL, [h as u64, wr.as_ptr() as u64, rd.as_ptr() as u64, deadline_ns, 0, 0])
        .map(|(a, b)| (a as usize, b as usize))
}

/// Blocks while the u32 at `addr` is `expected`, until a wake or `deadline_ns`.
pub fn futex_wait(addr: &core::sync::atomic::AtomicU32, expected: u32, deadline_ns: u64) -> Result<(), FErr> {
    call1(nr::FUTEX_WAIT, [addr.as_ptr() as u64, expected as u64, deadline_ns, 0, 0, 0]).map(|_| ())
}

/// Wakes up to `count` threads waiting on `addr`.
pub fn futex_wake(addr: &core::sync::atomic::AtomicU32, count: u32) -> u64 {
    call1(nr::FUTEX_WAKE, [addr.as_ptr() as u64, count as u64, 0, 0, 0, 0]).unwrap_or(0)
}

pub fn object_signal(h: u32, clear: u32, set: u32) -> Result<(), FErr> {
    call1(nr::OBJECT_SIGNAL, [h as u64, clear as u64, set as u64, 0, 0, 0]).map(|_| ())
}

pub fn vmo_read(h: u32, offset: u64, buf: &mut [u8]) -> Result<usize, FErr> {
    call1(nr::VMO_READ, [h as u64, offset, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0]).map(|n| n as usize)
}

/// A new process with `grants` (never beyond the caller's): (process, address space).
pub fn process_create(name: &str, grants: u32) -> Result<(u32, u32), FErr> {
    call(nr::PROCESS_CREATE, [name.as_ptr() as u64, name.len() as u64, grants as u64, 0, 0, 0])
        .map(|(p, a)| (p as u32, a as u32))
}

/// Starts `thread` at `entry` with `stack_top`; `bootstrap` (moved) becomes the child's handle 1.
pub fn process_start(process: u32, thread: u32, entry: u64, stack_top: u64, bootstrap: u32) -> Result<(), FErr> {
    call1(nr::PROCESS_START, [process as u64, thread as u64, entry, stack_top, bootstrap as u64, 0]).map(|_| ())
}

pub fn process_kill(process: u32) -> Result<(), FErr> {
    call1(nr::PROCESS_KILL, [process as u64, 0, 0, 0, 0, 0]).map(|_| ())
}

/// (pid, exited, exit code) of a process.
pub fn process_info(process: u32) -> Result<(u32, bool, i64), FErr> {
    let mut b = [0u8; 56];
    call1(nr::PROCESS_INFO, [process as u64, b.as_mut_ptr() as u64, b.len() as u64, 0, 0, 0])?;
    let u32_at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    let mut code = [0u8; 8];
    code.copy_from_slice(&b[16..24]);
    Ok((u32_at(0), u32_at(12) != 0, i64::from_le_bytes(code)))
}

pub fn thread_create(process: u32, name: &str) -> Result<u32, FErr> {
    call1(nr::THREAD_CREATE, [process as u64, name.as_ptr() as u64, name.len() as u64, 0, 0, 0]).map(|h| h as u32)
}

pub fn vmo_write(h: u32, offset: u64, data: &[u8]) -> Result<(), FErr> {
    call1(nr::VMO_WRITE, [h as u64, offset, data.as_ptr() as u64, data.len() as u64, 0, 0]).map(|_| ())
}

/// Waits for any of `items` ([handle, signals]); returns the index of one that's ready.
pub fn wait_many(items: &[[u32; 2]], deadline_ns: u64) -> Result<usize, FErr> {
    call1(nr::OBJECT_WAIT_MANY, [items.as_ptr() as u64, items.len() as u64, deadline_ns, 0, 0, 0]).map(|i| i as usize)
}

pub fn resource_create(parent: u32, kind: u64, base: u64, len: u64) -> Result<u32, FErr> {
    call1(nr::RESOURCE_CREATE, [parent as u64, kind, base, len, 0, 0]).map(|h| h as u32)
}

pub fn ioport_grant(resource: u32, port: u16, len: u16) -> Result<(), FErr> {
    call1(nr::IOPORT_GRANT, [resource as u64, port as u64, len as u64, 0, 0, 0]).map(|_| ())
}

/// One byte from an I/O port the process was granted.
#[inline]
pub fn inb(port: u16) -> u8 {
    let v: u8;
    // SAFETY: a port read; an ungranted port faults the process, nothing else.
    unsafe { core::arch::asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags)) };
    v
}

/// One byte to an I/O port the process was granted.
#[inline]
pub fn outb(port: u16, v: u8) {
    // SAFETY: as `inb`.
    unsafe { core::arch::asm!("out dx, al", in("al") v, in("dx") port, options(nomem, nostack, preserves_flags)) };
}

/// Device memory (MMIO covered by `resource`) as a VMO whose mappings use `cache`.
pub fn vmo_create_physical(resource: u32, phys: u64, len: u64, cache: u64) -> Result<u32, FErr> {
    call1(nr::VMO_CREATE_PHYSICAL, [resource as u64, phys, len, cache, 0, 0]).map(|h| h as u32)
}

/// Writes the UEFI variable `name` (conductor only: `FreallyBootCtl`, 32 bytes).
pub fn uefi_set_var(name: &str, data: &[u8]) -> Result<(), FErr> {
    call1(nr::UEFI_SET_VAR, [name.as_ptr() as u64, name.len() as u64, data.as_ptr() as u64, data.len() as u64, 0, 0])
        .map(|_| ())
}

/// A process's CPU time (ns its live threads have run) and the bytes mapped
/// in its address space (Monitor's columns).
pub fn process_usage(process: u32) -> Result<(u64, u64), FErr> {
    let mut b = [0u8; 72];
    call1(nr::PROCESS_INFO, [process as u64, b.as_mut_ptr() as u64, b.len() as u64, 0, 0, 0])?;
    let u = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap_or_default());
    Ok((u(56), u(64)))
}

/// Resets the machine (the boot-control grant: conductor). Returns only if refused.
pub fn system_reset() -> FErr {
    match call1(nr::SYSTEM_RESET, [0; 6]) {
        Ok(_) => FErr::Internal,
        Err(e) => e,
    }
}
