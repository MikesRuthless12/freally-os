// SPDX-License-Identifier: Apache-2.0
//! Time: the monotonic clock, durations, sleeping.

use crate::sys;
use core::sync::atomic::AtomicU32;
pub use core::time::Duration;
use fcore_abi::clock;

/// A point on the monotonic clock (ns since the kernel's clock started).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Instant(u64);

impl Instant {
    pub fn now() -> Instant {
        Instant(sys::clock_get(clock::MONOTONIC))
    }
    pub fn as_ns(&self) -> u64 {
        self.0
    }
    pub fn elapsed(&self) -> Duration {
        Duration::from_nanos(Instant::now().0.saturating_sub(self.0))
    }
    /// A deadline `d` from now, for calls that take one.
    pub fn after(d: Duration) -> u64 {
        Instant::now().0.saturating_add(d.as_nanos().min(u64::MAX as u128) as u64)
    }
}

/// Nanoseconds since the Unix epoch (UTC), from the RTC at boot and the
/// zone (`FreallyClock`, TASK-180A).
pub fn utc_ns() -> u64 {
    sys::clock_get(clock::UTC)
}

/// Local time, as nanoseconds since 1970 on the zone's clock: what a person
/// reads (the desktop's clock).
pub fn local_ns() -> u64 {
    sys::clock_get(clock::LOCAL)
}

/// A UTC time (ns) as local time: the zone's offset added (Files' dates).
pub fn to_local(utc_ns: u64) -> u64 {
    let off = crate::machine::zone().map_or(0, |(m, _)| m as i64 * 60_000_000_000);
    (utc_ns as i64).saturating_add(off).max(0) as u64
}

/// Sleeps for `d` (a futex wait nobody wakes).
pub fn sleep(d: Duration) {
    let word = AtomicU32::new(0);
    let deadline = Instant::after(d);
    while Instant::now().0 < deadline {
        if sys::futex_wait(&word, 0, deadline).is_err_and(|e| e != fcore_abi::FErr::TimedOut) {
            return;
        }
    }
}
