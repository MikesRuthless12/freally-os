// SPDX-License-Identifier: Apache-2.0
//! ftest: the in-OS test harness. Phase 1 holds the runtime checks the user
//! test program runs inside Freally OS (`checks`); later phases add the
//! harness services' own tests.
#![no_std]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use fstd::ipc::Channel;
use fstd::time::{Duration, Instant};
use fwire::proto::conductor::{FaultKind, FaultReport};

/// Runs each check in turn: 42 if all pass, else the failing check's number.
pub fn checks() -> i64 {
    let list: [fn() -> bool; 6] = [heap, wire, clock, channel, event, sleep];
    for (i, check) in list.iter().enumerate() {
        if !check() {
            fstd::error!("utest: check {} failed", i + 1);
            return i as i64 + 1;
        }
    }
    fstd::info!("utest: all {} checks pass", list.len());
    42
}

/// Small and large allocations, a map, growth and freeing.
fn heap() -> bool {
    let mut v: Vec<u64> = (0..10_000).collect();
    v.retain(|x| x % 3 == 0);
    let big = alloc::vec![7u8; 300_000];
    let mut m = BTreeMap::new();
    for i in 0..500u32 {
        m.insert(i, String::from("x").repeat((i % 40) as usize));
    }
    v.len() == 3334 && v.iter().sum::<u64>() == 16_668_333 && big.iter().all(|&b| b == 7) && m[&499].len() == 19
}

fn wire() -> bool {
    let r = FaultReport {
        seq: 1,
        ts_ns: 2,
        pid: 3,
        service: String::from("utest"),
        kind: FaultKind::Assert,
        rip: 4,
        addr: 5,
        message: String::from("ok"),
        frames: alloc::vec![6, 7],
        restarted: false,
    };
    let Ok(msg) = fwire::request(9, 7, &r) else { return false };
    fwire::parse(&msg).is_ok_and(|(h, p)| h.txid == 9 && fwire::from_payload::<FaultReport>(p).is_ok_and(|x| x == r))
}

fn clock() -> bool {
    let a = Instant::now();
    let mut spin = 0u64;
    while a.elapsed() < Duration::from_micros(200) {
        spin = spin.wrapping_add(1);
    }
    let b = Instant::now();
    b > a && b.as_ns() - a.as_ns() < 1_000_000_000 && spin > 0 && fstd::time::utc_ns() > 1_700_000_000_000_000_000
}

/// A message and a handle through a channel pair.
fn channel() -> bool {
    let Ok((a, b)) = Channel::pair() else { return false };
    let Ok((c, d)) = Channel::pair() else { return false };
    if a.send(b"ping", alloc::vec![c.0]).is_err() {
        return false;
    }
    let Ok(m) = b.recv(Instant::after(Duration::from_secs(1))) else { return false };
    if m.bytes != b"ping" || m.handles.len() != 1 {
        return false;
    }
    // The moved handle is the same channel: write through it, read from `d`.
    let moved = Channel(m.handles.into_iter().next().unwrap_or_else(|| unreachable!()));
    moved.send(b"pong", Vec::new()).is_ok()
        && d.recv(Instant::after(Duration::from_secs(1))).is_ok_and(|m| m.bytes == b"pong")
}

fn event() -> bool {
    let Ok(e) = fstd::sys::event_create() else { return false };
    let ok = fstd::sys::object_signal(e, 0, 1 << 25).is_ok()
        && fstd::sys::wait_one(e, 1 << 25, Instant::after(Duration::from_millis(100)))
            .is_ok_and(|s| s & (1 << 25) != 0)
        && fstd::sys::object_signal(e, 1, 0).is_err();
    let _ = fstd::sys::handle_close(e);
    ok
}

fn sleep() -> bool {
    let a = Instant::now();
    fstd::time::sleep(Duration::from_millis(2));
    a.elapsed() >= Duration::from_millis(2)
}
