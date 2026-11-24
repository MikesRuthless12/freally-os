// SPDX-License-Identifier: Apache-2.0
//! DHCP's client messages (RFC 2131) and the options netstack reads (RFC
//! 2132): DISCOVER and REQUEST out; OFFER, ACK and NAK in, each read into a
//! [`Reply`] with its lease. Every option's length is checked before it's read.

use crate::{Bad, Ipv4, Mac};
use alloc::vec::Vec;

pub const SERVER_PORT: u16 = 67;
pub const CLIENT_PORT: u16 = 68;

const MAGIC: [u8; 4] = [99, 130, 83, 99];

/// Message types (option 53).
pub mod kind {
    pub const DISCOVER: u8 = 1;
    pub const OFFER: u8 = 2;
    pub const REQUEST: u8 = 3;
    pub const ACK: u8 = 5;
    pub const NAK: u8 = 6;
}

/// What a server offered or granted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lease {
    pub address: Ipv4,
    pub server: Ipv4,
    /// The subnet mask's prefix length (24 for 255.255.255.0).
    pub prefix: u8,
    pub router: Option<Ipv4>,
    pub dns: Vec<Ipv4>,
    /// Seconds: the lease, and when to renew (T1, else half the lease).
    pub seconds: u32,
    pub renew: u32,
}

/// A server's answer to this client.
#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    Offer(Lease),
    Ack(Lease),
    Nak,
}

/// The fixed part every client message shares: a request, Ethernet, the
/// transaction id, the broadcast flag (no address yet), the client's MAC.
fn header(xid: u32, mac: Mac) -> Vec<u8> {
    let mut m = alloc::vec![0u8; 236];
    m[0] = 1;
    m[1] = 1;
    m[2] = 6;
    m[4..8].copy_from_slice(&xid.to_be_bytes());
    m[10] = 0x80;
    m[28..34].copy_from_slice(&mac);
    m.extend_from_slice(&MAGIC);
    m
}

/// The options both messages ask with: the parameter request list (mask,
/// router, DNS, lease, T1, T2), then the end.
fn finish(mut m: Vec<u8>) -> Vec<u8> {
    m.extend_from_slice(&[55, 6, 1, 3, 6, 51, 58, 59, 255]);
    m
}

/// DHCPDISCOVER: who will give this MAC an address?
pub fn discover(xid: u32, mac: Mac) -> Vec<u8> {
    let mut m = header(xid, mac);
    m.extend_from_slice(&[53, 1, kind::DISCOVER]);
    finish(m)
}

/// DHCPREQUEST for the address `offer` gave, from its server.
pub fn request(xid: u32, mac: Mac, offer: &Lease) -> Vec<u8> {
    let mut m = header(xid, mac);
    m.extend_from_slice(&[53, 1, kind::REQUEST, 50, 4]);
    m.extend_from_slice(&offer.address);
    m.extend_from_slice(&[54, 4]);
    m.extend_from_slice(&offer.server);
    finish(m)
}

fn prefix(mask: [u8; 4]) -> u8 {
    u32::from_be_bytes(mask).leading_ones() as u8
}

/// A server's message to this client (`xid`, `mac`): an offer, an ack or a
/// nak. Anything else, or for another client, is refused.
pub fn parse(m: &[u8], xid: u32, mac: Mac) -> Result<Reply, Bad> {
    if m.len() < 240 {
        return Err(Bad::Short);
    }
    if m[0] != 2 || m[4..8] != xid.to_be_bytes() || m[28..34] != mac || m[236..240] != MAGIC {
        return Err(Bad::Unsupported);
    }
    let mut lease = Lease { address: m[16..20].try_into().map_err(|_| Bad::Short)?, ..Lease::default() };
    let (mut kind, mut t1) = (0u8, None);
    let mut at = 240;
    while at < m.len() {
        let code = m[at];
        match code {
            0 => {
                at += 1;
                continue;
            }
            255 => break,
            _ => {}
        }
        let len = *m.get(at + 1).ok_or(Bad::Short)? as usize;
        let v = m.get(at + 2..at + 2 + len).ok_or(Bad::Short)?;
        let four = || -> Option<[u8; 4]> { v.get(..4)?.try_into().ok() };
        match code {
            53 if len == 1 => kind = v[0],
            1 => lease.prefix = four().map_or(0, prefix),
            3 => lease.router = four(),
            6 => lease.dns = v.as_chunks::<4>().0.to_vec(),
            51 => lease.seconds = four().map_or(0, u32::from_be_bytes),
            54 => lease.server = four().unwrap_or_default(),
            58 => t1 = four().map(u32::from_be_bytes),
            _ => {}
        }
        at += 2 + len;
    }
    lease.renew = t1.unwrap_or(lease.seconds / 2);
    match kind {
        kind::OFFER => Ok(Reply::Offer(lease)),
        kind::ACK => Ok(Reply::Ack(lease)),
        kind::NAK => Ok(Reply::Nak),
        _ => Err(Bad::Unsupported),
    }
}
