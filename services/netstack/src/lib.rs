// SPDX-License-Identifier: Apache-2.0
//! netstack's logic (TASK-174), tested on the host: the DHCP client's states
//! and timers, the ARP cache, and where a packet goes next. `main.rs` runs it
//! over the netdev protocol. Times are nanoseconds on one monotonic clock.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;
use fnet::dhcp::{self, Lease, Reply};
use fnet::{Ipv4, Mac};

const SECOND: u64 = 1_000_000_000;

/// Where DHCP stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dhcp {
    /// Asking who will give an address: the next DISCOVER's wait, in seconds.
    Discovering { wait: u64 },
    /// Asked the server for `offer` (or to keep `bound`, renewing).
    Requesting { offer: Lease, bound: Option<(Lease, u64)>, tries: u32 },
    /// `lease` held since `since`.
    Bound { lease: Lease, since: u64 },
}

/// The DHCP client (RFC 2131): DISCOVER at once, then after 2, 4, 8 … 64
/// s; REQUEST for the first offer; bound at the ACK; renewed at T1 by a new
/// REQUEST (keeping the address meanwhile); a NAK, or a request unanswered
/// three times, starts again.
#[derive(Debug)]
pub struct Client {
    pub xid: u32,
    pub mac: Mac,
    pub state: Dhcp,
    /// When [`Client::due`] next has something to send.
    pub next: u64,
}

impl Client {
    pub fn new(xid: u32, mac: Mac, now: u64) -> Client {
        Client { xid, mac, state: Dhcp::Discovering { wait: 2 }, next: now }
    }

    /// The address held, if any (kept while a renewal is asked).
    pub fn lease(&self) -> Option<&Lease> {
        match &self.state {
            Dhcp::Bound { lease, .. } => Some(lease),
            Dhcp::Requesting { bound: Some((lease, _)), .. } => Some(lease),
            _ => None,
        }
    }

    /// What to send now, if its time has come; the next time is set.
    pub fn due(&mut self, now: u64) -> Option<Vec<u8>> {
        if now < self.next {
            return None;
        }
        match &mut self.state {
            Dhcp::Discovering { wait } => {
                self.next = now + *wait * SECOND;
                *wait = (*wait * 2).min(64);
                Some(dhcp::discover(self.xid, self.mac))
            }
            Dhcp::Requesting { tries, .. } if *tries >= 3 => {
                self.state = Dhcp::Discovering { wait: 2 };
                self.next = now;
                self.due(now)
            }
            Dhcp::Requesting { offer, tries, .. } => {
                *tries += 1;
                self.next = now + 2 * SECOND;
                Some(dhcp::request(self.xid, self.mac, offer))
            }
            Dhcp::Bound { lease, since } => {
                let (lease, since) = (lease.clone(), *since);
                self.state = Dhcp::Requesting { offer: lease.clone(), bound: Some((lease, since)), tries: 0 };
                self.due(now)
            }
        }
    }

    /// A server's message (a UDP payload to port 68); what to send at once,
    /// if anything. Messages for another client, or out of turn, change nothing.
    pub fn reply(&mut self, payload: &[u8], now: u64) -> Option<Vec<u8>> {
        let reply = dhcp::parse(payload, self.xid, self.mac).ok()?;
        match (&self.state, reply) {
            (Dhcp::Discovering { .. }, Reply::Offer(offer)) => {
                self.state = Dhcp::Requesting { offer, bound: None, tries: 0 };
                self.next = now;
                self.due(now)
            }
            (Dhcp::Requesting { offer, .. }, Reply::Ack(lease)) if lease.address == offer.address => {
                let renew = if lease.renew == 0 { 3600 } else { lease.renew as u64 };
                self.next = now + renew * SECOND;
                self.state = Dhcp::Bound { lease, since: now };
                None
            }
            (Dhcp::Requesting { .. }, Reply::Nak) => {
                self.state = Dhcp::Discovering { wait: 2 };
                self.next = now;
                self.due(now)
            }
            _ => None,
        }
    }
}

/// A prefix length's mask.
fn mask(prefix: u8) -> u32 {
    if prefix == 0 { 0 } else { u32::MAX << (32 - prefix.min(32)) }
}

/// Where a packet for `dst` goes next: itself when it's on the link
/// (`ours`/`prefix`), else the router; `None` with no router.
pub fn next_hop(dst: Ipv4, ours: Ipv4, prefix: u8, router: Option<Ipv4>) -> Option<Ipv4> {
    let m = mask(prefix);
    if u32::from_be_bytes(dst) & m == u32::from_be_bytes(ours) & m { Some(dst) } else { router }
}

/// The ARP cache: at most [`ArpCache::MAX`] entries, each kept 5 minutes;
/// when full, the oldest goes.
#[derive(Debug, Default)]
pub struct ArpCache {
    entries: Vec<(Ipv4, Mac, u64)>,
}

impl ArpCache {
    pub const MAX: usize = 64;
    pub const KEEP: u64 = 300 * SECOND;

    pub fn learn(&mut self, ip: Ipv4, mac: Mac, now: u64) {
        self.entries.retain(|e| e.0 != ip);
        if self.entries.len() == Self::MAX
            && let Some(oldest) = self.entries.iter().enumerate().min_by_key(|(_, e)| e.2).map(|(i, _)| i)
        {
            self.entries.remove(oldest);
        }
        self.entries.push((ip, mac, now));
    }

    pub fn find(&self, ip: Ipv4, now: u64) -> Option<Mac> {
        self.entries.iter().find(|e| e.0 == ip && now.saturating_sub(e.2) < Self::KEEP).map(|e| e.1)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests;
