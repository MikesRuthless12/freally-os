// SPDX-License-Identifier: Apache-2.0
//! Futexes (syscalls 32–33): user-space locks block in the kernel only when contended.

use crate::sched::{self, ThreadRef};
use crate::sync::SpinLock;
use alloc::sync::Arc;
use alloc::vec::Vec;
use fcore_abi::FErr;

static WAITERS: SpinLock<Vec<(u64, u64, ThreadRef)>> = SpinLock::new(Vec::new());

fn space() -> u64 {
    crate::arch::cpu::cr3() & !0xFFF
}

/// Blocks while the u32 at user `addr` equals `expected` (until a wake or `deadline`).
pub fn wait(addr: u64, expected: u32, deadline: u64) -> Result<(), FErr> {
    if !addr.is_multiple_of(4) {
        return Err(FErr::InvalidArgs);
    }
    let me = sched::current().ok_or(FErr::BadState)?;
    let key = space();
    {
        let mut w = WAITERS.lock();
        let now: u32 = crate::mm::user::read_user(addr)?;
        if now != expected {
            return Err(FErr::ShouldWait);
        }
        w.push((key, addr, me.clone()));
    }
    sched::block_until(deadline);
    let mut w = WAITERS.lock();
    let still_waiting = w.iter().any(|(_, _, t)| Arc::ptr_eq(t, &me));
    w.retain(|(_, _, t)| !Arc::ptr_eq(t, &me));
    if sched::dying() {
        Err(FErr::BadState)
    } else if still_waiting {
        Err(FErr::TimedOut)
    } else {
        Ok(())
    }
}

/// Wakes up to `count` threads waiting on user `addr`; returns how many.
pub fn wake(addr: u64, count: u32) -> u64 {
    let key = space();
    let mut woken = Vec::new();
    {
        let mut w = WAITERS.lock();
        let mut i = 0;
        while i < w.len() && (woken.len() as u32) < count {
            if w[i].0 == key && w[i].1 == addr {
                woken.push(w.remove(i).2);
            } else {
                i += 1;
            }
        }
    }
    for t in &woken {
        sched::wake(t);
    }
    woken.len() as u64
}
