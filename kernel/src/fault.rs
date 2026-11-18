// SPDX-License-Identifier: Apache-2.0
//! Fault reports (TASK-031): a fault in user mode ends only that process and
//! files a report: who, what, where, and up to 16 return addresses from the
//! frame-pointer chain (user builds keep frame pointers). The kernel keeps the
//! last 32; conductor reads them (system_info FAULTS) to restart services.

use crate::arch::idt::Frame;
use crate::sync::SpinLock;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

pub struct FaultReport {
    pub seq: u64,
    pub ts_ns: u64,
    pub pid: u32,
    pub service: String,
    pub kind: u8,
    pub rip: u64,
    pub addr: u64,
    pub message: String,
    pub frames: Vec<u64>,
}

static RING: SpinLock<VecDeque<FaultReport>> = SpinLock::new(VecDeque::new());
static SEQ: AtomicU64 = AtomicU64::new(1);

/// The report kinds (PRD § 3): Panic 0, PageFault 1, GeneralProtection 2,
/// InvalidOpcode 3, DivideError 4, Watchdog 5, Assert 6, IpcProtocol 7.
fn kind_of(vector: u64) -> u8 {
    match vector {
        14 => 1,
        6 => 3,
        0 => 4,
        _ => 2,
    }
}

/// Walks the user's frame-pointer chain (each step copied safely).
fn frames_from(rbp: u64) -> Vec<u64> {
    let mut out = Vec::new();
    let mut fp = rbp;
    while out.len() < 16 && fp != 0 && fp.is_multiple_of(8) {
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

/// A fault in ring 3: file the report and end the process. Never returns.
pub fn user_fault(f: &Frame) -> ! {
    let addr = if f.vector == 14 { crate::arch::cpu::cr2() } else { 0 };
    let p = crate::proc::current();
    let (pid, service) = p
        .as_ref()
        .and_then(|p| crate::proc::as_process(p).ok().map(|x| (x.pid, String::from(x.name()))))
        .unwrap_or((0, String::from("?")));
    let message = alloc::format!("{} at {:#x}", crate::exceptions::name(f.vector), f.rip);
    let mut frames = alloc::vec![f.rip];
    frames.extend(frames_from(f.rbp));
    let r = FaultReport {
        seq: SEQ.fetch_add(1, Ordering::Relaxed),
        ts_ns: crate::time::now_ns(),
        pid,
        service,
        kind: kind_of(f.vector),
        rip: f.rip,
        addr,
        message,
        frames,
    };
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
    if let Some(p) = p {
        crate::proc::terminate(&p, -11);
    }
    crate::sched::exit_current();
}

/// Every report, encoded: seq u64, ts u64, pid u32, kind u8, rip u64, addr u64,
/// then the service, the message (each u16-length-prefixed) and the frames (u16 count).
pub fn encode_all() -> Vec<u8> {
    let ring = RING.lock();
    let mut out = Vec::new();
    for r in ring.iter() {
        out.extend_from_slice(&r.seq.to_le_bytes());
        out.extend_from_slice(&r.ts_ns.to_le_bytes());
        out.extend_from_slice(&r.pid.to_le_bytes());
        out.push(r.kind);
        out.extend_from_slice(&r.rip.to_le_bytes());
        out.extend_from_slice(&r.addr.to_le_bytes());
        for s in [r.service.as_bytes(), r.message.as_bytes()] {
            out.extend_from_slice(&(s.len() as u16).to_le_bytes());
            out.extend_from_slice(s);
        }
        out.extend_from_slice(&(r.frames.len() as u16).to_le_bytes());
        for fr in &r.frames {
            out.extend_from_slice(&fr.to_le_bytes());
        }
    }
    out
}

/// How many reports the ring holds.
pub fn count() -> usize {
    RING.lock().len()
}

/// The newest report's message.
pub fn latest_message() -> Option<String> {
    RING.lock().back().map(|r| r.message.clone())
}
