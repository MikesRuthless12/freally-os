// SPDX-License-Identifier: Apache-2.0
//! The inventory's other half (TASK-063, FR-040), in e2e images only: how
//! many times each request ordinal reached this process, and the answer to
//! `fwire::proto::inventory::ASK`, given here and never passed on.
//! `Channel::try_recv` calls `seen` for every message; a service reading
//! its channel some other way calls it itself.

use crate::ipc::Channel;
use alloc::vec::Vec;
use core::cell::RefCell;
use fwire::proto::inventory as inv;

struct Counts(RefCell<Vec<(u32, u32)>>);
// SAFETY: a Freally process runs one thread (TASK-029); nothing else reaches this.
unsafe impl Sync for Counts {}

static COUNTS: Counts = Counts(RefCell::new(Vec::new()));

/// A message just read from `ch`: a request is counted; the inventory's
/// question is answered (true: skip it). Off outside e2e images.
pub fn seen(ch: &Channel, bytes: &[u8]) -> bool {
    seen_if(crate::service::flavour() == 2, ch, bytes)
}

/// As `seen`, for a process that knows its flavour itself (conductor).
pub fn seen_if(e2e: bool, ch: &Channel, bytes: &[u8]) -> bool {
    if !e2e {
        return false;
    }
    let Ok((h, _)) = fwire::parse(bytes) else { return false };
    if h.flags != 0 {
        return false; // replies and events
    }
    if h.ordinal == inv::ASK {
        let ordinals = counts();
        if let Ok(m) = fwire::reply(h.txid, h.ordinal, &inv::Served { ordinals }) {
            let _ = ch.send(&m, Vec::new());
        }
        return true;
    }
    tally(h.ordinal);
    false
}

fn tally(ordinal: u32) {
    let mut c = COUNTS.0.borrow_mut();
    match c.iter_mut().find(|(o, _)| *o == ordinal) {
        Some(e) => e.1 = e.1.saturating_add(1),
        None => c.push((ordinal, 1)),
    }
}

/// A one-way message the service took (sent as an event, so `seen` passes
/// it by): counted where it's handled.
pub fn count(ordinal: u32) {
    if crate::service::flavour() == 2 {
        tally(ordinal);
    }
}

/// This process's counts so far.
pub fn counts() -> Vec<(u32, u32)> {
    COUNTS.0.borrow().clone()
}
