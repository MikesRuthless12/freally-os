// SPDX-License-Identifier: Apache-2.0
//! Channels, events and waits (TASK-028).
//!
//! - A channel is two ends; each end reads what the other writes. A message is
//!   at most 65,536 bytes and 16 handles; handles are moved (they need TRANSFER).
//! - A read that's too small fails with `BufferTooSmall` and leaves the message.
//! - Closing an end tells the other (`PEER_CLOSED`); writing to a closed peer
//!   fails with `PeerClosed`.
//! - Waits block until a signal or a deadline; `channel_call` writes, then waits
//!   for the reply, waking the server first so it runs next.

use crate::obj::handles::Handle;
use crate::obj::{Header, signal};
use crate::sched;
use crate::sync::SpinLock;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use fcore_abi::FErr;

pub const MAX_BYTES: usize = 65_536;
pub const MAX_HANDLES: usize = 16;
/// Messages waiting in one direction before writes fail with `Busy`.
pub const MAX_QUEUED: usize = 4096;

pub struct Message {
    pub bytes: Vec<u8>,
    pub handles: Vec<Handle>,
}

struct Pair {
    /// `inbox[i]`: what end `i` reads.
    inbox: [SpinLock<VecDeque<Message>>; 2],
    headers: [Header; 2],
    closed: [AtomicBool; 2],
}

pub struct ChannelEnd {
    pair: Arc<Pair>,
    side: usize,
}

impl ChannelEnd {
    pub fn header(&self) -> &Header {
        &self.pair.headers[self.side]
    }
}

impl Drop for ChannelEnd {
    fn drop(&mut self) {
        self.pair.closed[self.side].store(true, Ordering::Release);
        self.pair.headers[1 - self.side].update(signal::WRITABLE, signal::PEER_CLOSED);
    }
}

pub fn channel_pair() -> (ChannelEnd, ChannelEnd) {
    let pair = Arc::new(Pair {
        inbox: [SpinLock::new(VecDeque::new()), SpinLock::new(VecDeque::new())],
        headers: [Header::new(), Header::new()],
        closed: [AtomicBool::new(false), AtomicBool::new(false)],
    });
    pair.headers[0].update(0, signal::WRITABLE);
    pair.headers[1].update(0, signal::WRITABLE);
    (ChannelEnd { pair: pair.clone(), side: 0 }, ChannelEnd { pair, side: 1 })
}

pub fn write(end: &ChannelEnd, msg: Message) -> Result<(), FErr> {
    if msg.bytes.len() > MAX_BYTES || msg.handles.len() > MAX_HANDLES {
        return Err(FErr::InvalidArgs);
    }
    let peer = 1 - end.side;
    if end.pair.closed[peer].load(Ordering::Acquire) {
        return Err(FErr::PeerClosed);
    }
    {
        let mut q = end.pair.inbox[peer].lock();
        if q.len() >= MAX_QUEUED {
            return Err(FErr::Busy);
        }
        q.push_back(msg);
    }
    end.pair.headers[peer].update(0, signal::READABLE);
    Ok(())
}

/// Reads one message if it fits `max_bytes` and `max_handles`; otherwise
/// `Err((BufferTooSmall, bytes, handles))` says what it needs.
pub fn read(end: &ChannelEnd, max_bytes: usize, max_handles: usize) -> Result<Message, (FErr, usize, usize)> {
    let mut q = end.pair.inbox[end.side].lock();
    let Some(m) = q.front() else {
        let peer_gone = end.pair.closed[1 - end.side].load(Ordering::Acquire);
        return Err((if peer_gone { FErr::PeerClosed } else { FErr::ShouldWait }, 0, 0));
    };
    if m.bytes.len() > max_bytes || m.handles.len() > max_handles {
        return Err((FErr::BufferTooSmall, m.bytes.len(), m.handles.len()));
    }
    let m = q.pop_front().expect("checked above");
    let empty = q.is_empty();
    drop(q);
    if empty {
        end.pair.headers[end.side].update(signal::READABLE, 0);
    }
    Ok(m)
}

/// Waits for any of `signals` on `h`; returns what was observed, or `TimedOut`
/// when `deadline` (TSC ticks; u64::MAX is forever) passes first.
pub fn wait_one(h: &Header, signals: u32, deadline: u64) -> Result<u32, FErr> {
    loop {
        let me = sched::current().ok_or(FErr::BadState)?;
        h.waiters.lock().push(me.clone());
        let now = h.get();
        if now & signals != 0 {
            h.waiters.lock().retain(|t| !Arc::ptr_eq(t, &me));
            return Ok(now);
        }
        if deadline != u64::MAX && crate::arch::cpu::rdtsc() >= deadline {
            h.waiters.lock().retain(|t| !Arc::ptr_eq(t, &me));
            return Err(FErr::TimedOut);
        }
        sched::block_until(deadline);
        h.waiters.lock().retain(|t| !Arc::ptr_eq(t, &me));
    }
}

/// Waits on several headers; returns the index of the first satisfied.
pub fn wait_many(items: &[(&Header, u32)], deadline: u64) -> Result<usize, FErr> {
    loop {
        let me = sched::current().ok_or(FErr::BadState)?;
        for (h, _) in items {
            h.waiters.lock().push(me.clone());
        }
        let hit = items.iter().position(|(h, s)| h.get() & s != 0);
        let timed_out = deadline != u64::MAX && crate::arch::cpu::rdtsc() >= deadline;
        if hit.is_none() && !timed_out {
            sched::block_until(deadline);
        }
        for (h, _) in items {
            h.waiters.lock().retain(|t| !Arc::ptr_eq(t, &me));
        }
        if let Some(i) = hit.or_else(|| items.iter().position(|(h, s)| h.get() & s != 0)) {
            return Ok(i);
        }
        if deadline != u64::MAX && crate::arch::cpu::rdtsc() >= deadline {
            return Err(FErr::TimedOut);
        }
    }
}

/// Writes `msg`, then waits for and returns the reply.
pub fn call(
    end: &ChannelEnd,
    msg: Message,
    max_bytes: usize,
    max_handles: usize,
    deadline: u64,
) -> Result<Message, (FErr, usize, usize)> {
    write(end, msg).map_err(|e| (e, 0, 0))?;
    // The server runs next (it was woken by the write); then wait for its reply.
    sched::yield_now();
    loop {
        match read(end, max_bytes, max_handles) {
            Err((FErr::ShouldWait, _, _)) => {
                wait_one(end.header(), signal::READABLE | signal::PEER_CLOSED, deadline).map_err(|e| (e, 0, 0))?;
            }
            other => return other,
        }
    }
}

ktest!(ipc_messages_arrive_and_peer_closed_is_reported, {
    let (a, b) = channel_pair();
    write(&a, Message { bytes: alloc::vec![1, 2, 3], handles: Vec::new() })?;
    if b.header().get() & signal::READABLE == 0 {
        return Err("the reader wasn't signalled");
    }
    match read(&b, 2, 0) {
        Err((FErr::BufferTooSmall, 3, 0)) => {}
        _ => return Err("a too-small read didn't say what it needs"),
    }
    let m = read(&b, 64, 0).map_err(|_| "read")?;
    if m.bytes != [1, 2, 3] {
        return Err("the bytes changed in flight");
    }
    if read(&b, 64, 0).err().map(|e| e.0) != Some(FErr::ShouldWait) {
        return Err("an empty channel didn't say ShouldWait");
    }
    drop(a);
    if b.header().get() & signal::PEER_CLOSED == 0 {
        return Err("closing an end didn't signal PEER_CLOSED");
    }
    if write(&b, Message { bytes: Vec::new(), handles: Vec::new() }).err() != Some(FErr::PeerClosed) {
        return Err("writing to a closed peer didn't fail with PeerClosed");
    }
    Ok(())
});

ktest!(ipc_limits_are_enforced, {
    let (a, _b) = channel_pair();
    let big = Message { bytes: alloc::vec![0; MAX_BYTES + 1], handles: Vec::new() };
    if write(&a, big).err() != Some(FErr::InvalidArgs) {
        return Err("a message over 64 KiB was accepted");
    }
    Ok(())
});

/// `cargo xtask bench ipc`: kernel threads ping-pong over a channel pair.
pub fn bench() -> u64 {
    use core::sync::atomic::AtomicU64;
    static RTT: AtomicU64 = AtomicU64::new(0);
    static ENDS: core::sync::atomic::AtomicPtr<(ChannelEnd, ChannelEnd)> =
        core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());
    extern "C" fn server(_: u64) {
        // SAFETY: set (to a leaked pair) before this thread starts.
        let (_, b) = unsafe { &*ENDS.load(Ordering::Acquire) };
        loop {
            match read(b, 64, 0) {
                Ok(m) => {
                    let _ = write(b, m);
                }
                Err((FErr::ShouldWait, _, _)) => {
                    let _ = wait_one(b.header(), signal::READABLE | signal::PEER_CLOSED, u64::MAX);
                }
                Err(_) => return,
            }
        }
    }
    ENDS.store(alloc::boxed::Box::leak(alloc::boxed::Box::new(channel_pair())), Ordering::Release);
    if let Ok(t) = sched::create("bench-server", sched::Class::Interactive, server, 0, false) {
        sched::start(&t);
    }
    // SAFETY: as above.
    let (a, _) = unsafe { &*ENDS.load(Ordering::Acquire) };
    let mut samples = alloc::vec![0u64; 10_000];
    crate::arch::cpu::interrupts_on();
    for s in samples.iter_mut() {
        let t0 = crate::arch::cpu::rdtsc();
        let _ = call(a, Message { bytes: alloc::vec![7; 16], handles: Vec::new() }, 64, 0, u64::MAX);
        *s = crate::arch::cpu::rdtsc() - t0;
    }
    crate::arch::cpu::interrupts_off();
    samples.sort_unstable();
    let median = crate::time::ticks_to_ns(samples[samples.len() / 2]);
    RTT.store(median, Ordering::Relaxed);
    median
}
