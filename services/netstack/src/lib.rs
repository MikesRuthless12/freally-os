// SPDX-License-Identifier: Apache-2.0
//! netstack's logic (TASK-174), tested on the host: the DHCP client's states
//! and timers, the ARP cache, and where a packet goes next. `main.rs` runs it
//! over the netdev protocol. Times are nanoseconds on one monotonic clock.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod resolver;
#[cfg(test)]
mod resolver_tests;
pub mod tcp;
#[cfg(test)]
mod tcp_tests;

use alloc::vec::Vec;
use fnet::dhcp::{self, Lease, Reply};
use fnet::ipv6::Ipv6;
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

/// A neighbour cache (ARP's for IPv4, NDP's for IPv6): at most
/// [`Neighbors::MAX`] entries, each kept 5 minutes; when full, the oldest goes.
#[derive(Debug)]
pub struct Neighbors<A> {
    entries: Vec<(A, Mac, u64)>,
}

impl<A> Default for Neighbors<A> {
    fn default() -> Self {
        Neighbors { entries: Vec::new() }
    }
}

/// IPv4's neighbours, by ARP.
pub type ArpCache = Neighbors<Ipv4>;

impl<A: Copy + PartialEq> Neighbors<A> {
    pub const MAX: usize = 64;
    pub const KEEP: u64 = 300 * SECOND;

    pub fn learn(&mut self, ip: A, mac: Mac, now: u64) {
        self.entries.retain(|e| e.0 != ip);
        if self.entries.len() == Self::MAX
            && let Some(oldest) = self.entries.iter().enumerate().min_by_key(|(_, e)| e.2).map(|(i, _)| i)
        {
            self.entries.remove(oldest);
        }
        self.entries.push((ip, mac, now));
    }

    pub fn find(&self, ip: A, now: u64) -> Option<Mac> {
        self.entries.iter().find(|e| e.0 == ip && now.saturating_sub(e.2) < Self::KEEP).map(|e| e.1)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// What SLAAC asks to send next.
#[derive(Debug, PartialEq, Eq)]
pub enum Slaac6 {
    /// A neighbour solicitation from :: for `target` (duplicate address detection).
    Dad(Ipv6),
    /// A router solicitation from the link-local address.
    RouterSolicitation,
}

/// IPv6 by SLAAC (RFC 4862): a link-local address from a random interface
/// id (never the MAC), checked by DAD (one solicitation, a second's wait),
/// then up to three router solicitations 4 s apart; each autonomous /64 a
/// router advertises gives a global address, checked by DAD too.
#[derive(Debug)]
pub struct Slaac {
    pub iid: [u8; 8],
    pub link_local: Ipv6,
    /// Addresses in use, and the one being checked (with when its check ends).
    pub addresses: Vec<(Ipv6, u8)>,
    checking: Option<(Ipv6, u8, u64)>,
    pending: Vec<(Ipv6, u8)>,
    pub router: Option<(Ipv6, Mac)>,
    pub dns: Vec<Ipv6>,
    solicitations: u32,
    /// When [`Slaac::due`] next has something to do (`u64::MAX`: nothing).
    pub next: u64,
    /// An address someone else holds (DAD failed): IPv6 is off.
    pub conflict: Option<Ipv6>,
}

impl Slaac {
    pub fn new(iid: [u8; 8], now: u64) -> Slaac {
        let ll = fnet::ipv6::link_local(iid);
        Slaac {
            iid,
            link_local: ll,
            addresses: Vec::new(),
            checking: None,
            pending: alloc::vec![(ll, 64)],
            router: None,
            dns: Vec::new(),
            solicitations: 0,
            next: now,
            conflict: None,
        }
    }

    /// Whether `a` is one of ours (in use).
    pub fn ours(&self, a: &Ipv6) -> bool {
        self.addresses.iter().any(|(x, _)| x == a)
    }

    /// What to send now, if its time has come; the next time is set.
    pub fn due(&mut self, now: u64) -> Option<Slaac6> {
        if self.conflict.is_some() || now < self.next {
            return None;
        }
        // A check that ran its second without an answer: the address is ours.
        if let Some((a, p, until)) = self.checking
            && now >= until
        {
            self.checking = None;
            self.addresses.push((a, p));
            if a == self.link_local {
                self.next = now;
            }
        }
        if self.checking.is_none()
            && let Some((a, p)) = self.pending.pop()
        {
            self.checking = Some((a, p, now + SECOND));
            self.next = now + SECOND;
            return Some(Slaac6::Dad(a));
        }
        if self.checking.is_some() {
            return None;
        }
        if self.router.is_none() && self.ours(&self.link_local) && self.solicitations < 3 {
            self.solicitations += 1;
            self.next = now + 4 * SECOND;
            return Some(Slaac6::RouterSolicitation);
        }
        self.next = u64::MAX;
        None
    }

    /// A neighbour advertisement for `target`: a duplicate, if we were checking it.
    pub fn advertised(&mut self, target: &Ipv6) {
        if self.checking.is_some_and(|(a, _, _)| a == *target) || (self.ours(target) && *target == self.link_local) {
            self.conflict = Some(*target);
            self.checking = None;
            self.next = u64::MAX;
        }
    }

    /// A router's advertisement, from `src`: its prefixes make addresses (checked first).
    pub fn router_ad(&mut self, src: Ipv6, ad: &fnet::ipv6::RouterAd, now: u64) {
        if ad.router_lifetime_s > 0 {
            self.router = ad.mac.map(|m| (src, m)).or(self.router);
        }
        if !ad.dns.is_empty() {
            self.dns = ad.dns.clone();
        }
        for (prefix, plen, _) in &ad.prefixes {
            let mut a = *prefix;
            a[8..].copy_from_slice(&self.iid);
            let known =
                self.ours(&a) || self.pending.iter().any(|(x, _)| *x == a) || self.checking.is_some_and(|c| c.0 == a);
            if !known {
                self.pending.push((a, *plen));
                self.next = self.next.min(now);
            }
        }
    }

    /// Whether `dst` is on the link: link-local, or inside one of our prefixes.
    pub fn on_link(&self, dst: &Ipv6) -> bool {
        dst[..2] == [0xFE, 0x80] || self.addresses.iter().any(|(a, p)| *p == 64 && a[..8] == dst[..8])
    }

    /// The address to send to `dst` from: link-local for link-local, else a global one.
    pub fn source_for(&self, dst: &Ipv6) -> Option<Ipv6> {
        if dst[..2] == [0xFE, 0x80] {
            return self.ours(&self.link_local).then_some(self.link_local);
        }
        self.addresses.iter().map(|(a, _)| *a).find(|a| a[..2] != [0xFE, 0x80])
    }
}

#[cfg(test)]
mod tests;
