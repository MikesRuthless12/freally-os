// SPDX-License-Identifier: Apache-2.0
//! Fault reports (TASK-031, PRD § 3).
//!
//! - A fault in user mode ends only that process and files a `FaultReport`:
//!   who, what, where, and up to 16 return addresses from the frame-pointer
//!   chain (user builds keep frame pointers). An `frt` panic arrives as a `ud2`
//!   with `USER_PANIC_MAGIC` in RAX and its message in RDI/RSI.
//! - The kernel keeps the last 32 and signals conductor's faults event (user
//!   bit 24); conductor reads them with `system_info(FAULTS)`, fwire-encoded.
//! - A kernel panic writes the last-panic page (a reserved physical page,
//!   magic + CRC32C); the next boot finds it, files it as a report from
//!   `freally-core`, and clears it.

use crate::arch::idt::Frame;
use crate::obj::{Header, Object, ObjectRef};
use crate::sync::SpinLock;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use fcore_abi::layout::{DIRECT_MAP, LAST_PANIC_PHYS};
use fwire::proto::conductor::{FaultKind, FaultReport, Faults};

static RING: SpinLock<VecDeque<FaultReport>> = SpinLock::new(VecDeque::new());
static SEQ: AtomicU64 = AtomicU64::new(1);
static EVENT: SpinLock<Option<ObjectRef>> = SpinLock::new(None);
/// The faults event's bit: set when a report is filed; conductor clears it.
pub const NEW_FAULT: u32 = 1 << 24;
const MAX_MESSAGE: usize = 512;

/// The event conductor waits on.
pub fn event() -> ObjectRef {
    EVENT.lock().get_or_insert_with(|| Arc::new(Object::Event(Header::new()))).clone()
}

fn kind_of(vector: u64) -> FaultKind {
    match vector {
        14 => FaultKind::PageFault,
        6 => FaultKind::InvalidOpcode,
        0 => FaultKind::DivideError,
        _ => FaultKind::GeneralProtection,
    }
}

/// Walks the user's frame-pointer chain (each step copied through the safe path).
fn frames_from(rbp: u64) -> Vec<u64> {
    let mut out = Vec::new();
    let mut fp = rbp;
    while out.len() < 15 && fp != 0 && fp.is_multiple_of(8) {
        let Ok(pair) = crate::mm::user::read_user::<[u64; 2]>(fp) else { break };
        if pair[1] == 0 {
            break;
        }
        out.push(pair[1]);
        if pair[0] <= fp {
            break;
        }
        fp = pair[0];
    }
    out
}

fn file(mut r: FaultReport) {
    r.seq = SEQ.fetch_add(1, Ordering::Relaxed);
    klog!(
        "freally-core: fault in {} (pid {}): {}; address {:#x}; {} frames",
        r.service,
        r.pid,
        r.message,
        r.addr,
        r.frames.len()
    );
    {
        let mut ring = RING.lock();
        if ring.len() == 32 {
            ring.pop_front();
        }
        ring.push_back(r);
    }
    if let Ok(h) = crate::obj::waitable(&event()) {
        h.update(0, NEW_FAULT);
    }
}

/// A fault in ring 3: file the report and end the process. The thread ends on
/// its way back out of the exception (`idt::dispatch`).
pub fn user_fault(f: &Frame) {
    let p = crate::proc::current();
    let (pid, service) = p
        .as_ref()
        .and_then(|p| crate::proc::as_process(p).ok().map(|x| (x.pid, String::from(x.name()))))
        .unwrap_or((0, String::from("?")));
    let panicked = f.vector == 6 && f.rax == fcore_abi::USER_PANIC_MAGIC;
    let (kind, message) = if panicked {
        let mut b = alloc::vec![0u8; (f.rsi as usize).min(MAX_MESSAGE)];
        let text = match crate::mm::user::copy_from_user(&mut b, f.rdi) {
            Ok(()) => String::from_utf8_lossy(&b).into_owned(),
            Err(_) => String::from("(the panic message couldn't be read)"),
        };
        (FaultKind::Panic, text)
    } else {
        (kind_of(f.vector), alloc::format!("{} at {:#x}", crate::exceptions::name(f.vector), f.rip))
    };
    let mut frames = alloc::vec![f.rip];
    frames.extend(frames_from(f.rbp));
    file(FaultReport {
        seq: 0,
        ts_ns: crate::time::now_ns(),
        pid,
        service,
        kind,
        rip: f.rip,
        addr: if f.vector == 14 { crate::arch::cpu::cr2() } else { 0 },
        message,
        frames,
        restarted: false,
    });
    if let Some(p) = p {
        crate::proc::terminate(&p, -11);
    }
    crate::sched::kill_current();
}

/// Every report, as `system_info(FAULTS)` returns them.
pub fn encode_all() -> Vec<u8> {
    let reports: Vec<FaultReport> = RING.lock().iter().cloned().collect();
    fwire::to_payload(&Faults { reports, next_cursor: 0 })
}

pub fn latest() -> Option<FaultReport> {
    RING.lock().back().cloned()
}

pub fn latest_seq() -> u64 {
    RING.lock().back().map_or(0, |r| r.seq)
}

// The last-panic page: magic u64, length u32, CRC32C u32 (over the rest),
// TSC u64, then the message (≤ 512 bytes, UTF-8).
const PANIC_MAGIC: u64 = u64::from_le_bytes(*b"FLASTPNC");
static PAGE_OK: AtomicBool = AtomicBool::new(false);

fn page() -> *mut u8 {
    (LAST_PANIC_PHYS + DIRECT_MAP) as *mut u8
}

/// At boot, before anything else allocates: claims the page if the memory map
/// left it free, then files what the last boot left there (if anything).
pub fn claim_last_panic_page() {
    if !crate::mm::frames::reserve(LAST_PANIC_PHYS) {
        klog!("freally-core: the last-panic page is in use this boot; a panic won't survive a restart");
        return;
    }
    PAGE_OK.store(true, Ordering::Release);
    // SAFETY: the frame is ours now and mapped by the direct map.
    let raw = unsafe { core::slice::from_raw_parts_mut(page(), 4096) };
    let magic = u64::from_le_bytes(raw[0..8].try_into().unwrap_or_default());
    let len = u32::from_le_bytes(raw[8..12].try_into().unwrap_or_default()) as usize;
    let crc = u32::from_le_bytes(raw[12..16].try_into().unwrap_or_default());
    if magic == PANIC_MAGIC && len <= MAX_MESSAGE && fcrypto::crc32c::crc32c(&raw[16..24 + len]) == crc {
        let message = String::from_utf8_lossy(&raw[24..24 + len]).into_owned();
        klog!("freally-core: the last boot stopped: {message}");
        file(FaultReport {
            seq: 0,
            ts_ns: 0,
            pid: 0,
            service: String::from("freally-core"),
            kind: FaultKind::Panic,
            rip: 0,
            addr: 0,
            message,
            frames: Vec::new(),
            restarted: true,
        });
    }
    raw[..24 + MAX_MESSAGE].fill(0);
}

/// Called by the panic screen: writes the page (no allocation, no locks).
pub fn write_last_panic(what: &str, detail: &str) {
    if !PAGE_OK.load(Ordering::Acquire) {
        return;
    }
    // SAFETY: the page was claimed at boot.
    let raw = unsafe { core::slice::from_raw_parts_mut(page(), 4096) };
    let mut n = 0;
    for part in [what, ". ", detail] {
        let mut take = part.len().min(MAX_MESSAGE - n);
        while !part.is_char_boundary(take) {
            take -= 1;
        }
        raw[24 + n..24 + n + take].copy_from_slice(&part.as_bytes()[..take]);
        n += take;
    }
    raw[16..24].copy_from_slice(&crate::arch::cpu::rdtsc().to_le_bytes());
    raw[8..12].copy_from_slice(&(n as u32).to_le_bytes());
    let crc = fcrypto::crc32c::crc32c(&raw[16..24 + n]);
    raw[12..16].copy_from_slice(&crc.to_le_bytes());
    raw[0..8].copy_from_slice(&PANIC_MAGIC.to_le_bytes());
}

ktest!(fault_the_last_panic_page_round_trips, {
    if !PAGE_OK.load(Ordering::Acquire) {
        return Err("the last-panic page wasn't claimed");
    }
    write_last_panic("A test panic", "At nowhere:1.");
    // Read it back as the next boot would (without filing it twice: then clear).
    // SAFETY: as above.
    let raw = unsafe { core::slice::from_raw_parts_mut(page(), 4096) };
    let len = u32::from_le_bytes(raw[8..12].try_into().unwrap_or_default()) as usize;
    let ok = &raw[24..24 + len] == b"A test panic. At nowhere:1."
        && fcrypto::crc32c::crc32c(&raw[16..24 + len])
            == u32::from_le_bytes(raw[12..16].try_into().unwrap_or_default());
    raw[..24 + MAX_MESSAGE].fill(0);
    if ok { Ok(()) } else { Err("the last-panic page didn't read back") }
});
