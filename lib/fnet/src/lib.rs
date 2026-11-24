// SPDX-License-Identifier: Apache-2.0
//! fnet (TASK-174, FR-060): the network's wire formats, read and written.
//! Every reader checks every length before it reads; anything malformed is
//! refused with its reason ([`Bad`]), never trusted. Written from the RFCs:
//! Ethernet II, ARP (RFC 826), IPv4 (RFC 791, 1122), ICMP (RFC 792), UDP
//! (RFC 768), the Internet checksum (RFC 1071), DHCP (RFC 2131, 2132).
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod dhcp;
pub mod dns;
pub mod ipv6;
pub mod tcp;

use alloc::vec::Vec;

/// Why a packet was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bad {
    Short,
    Version,
    HeaderLength,
    TotalLength,
    Checksum,
    Fragment,
    Unsupported,
}

pub type Mac = [u8; 6];
pub type Ipv4 = [u8; 4];

pub const BROADCAST: Mac = [0xFF; 6];
pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_ARP: u16 = 0x0806;
pub const PROTO_ICMP: u8 = 1;
pub const PROTO_UDP: u8 = 17;

fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

/// The Internet checksum (RFC 1071) of `data`, starting from `sum`.
pub fn checksum(data: &[u8], mut sum: u32) -> u16 {
    let (pairs, rest) = data.as_chunks::<2>();
    for c in pairs {
        sum += u16::from_be_bytes(*c) as u32;
    }
    if let [last] = rest {
        sum += (*last as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// The UDP pseudo-header's sum (RFC 768): the addresses, the protocol, the length.
pub(crate) fn pseudo(src: Ipv4, dst: Ipv4, proto: u8, len: u16) -> u32 {
    u16::from_be_bytes([src[0], src[1]]) as u32
        + u16::from_be_bytes([src[2], src[3]]) as u32
        + u16::from_be_bytes([dst[0], dst[1]]) as u32
        + u16::from_be_bytes([dst[2], dst[3]]) as u32
        + proto as u32
        + len as u32
}

/// An Ethernet II frame: its addresses, its type and its payload.
#[derive(Debug, PartialEq, Eq)]
pub struct Eth<'a> {
    pub dst: Mac,
    pub src: Mac,
    pub ethertype: u16,
    pub payload: &'a [u8],
}

impl<'a> Eth<'a> {
    pub fn parse(f: &'a [u8]) -> Result<Eth<'a>, Bad> {
        if f.len() < 14 {
            return Err(Bad::Short);
        }
        Ok(Eth {
            dst: f[0..6].try_into().map_err(|_| Bad::Short)?,
            src: f[6..12].try_into().map_err(|_| Bad::Short)?,
            ethertype: be16(f, 12),
            payload: &f[14..],
        })
    }
}

/// A frame from `src` to `dst` of `ethertype`, padded to Ethernet's 60 bytes.
pub fn eth(dst: Mac, src: Mac, ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity((14 + payload.len()).max(60));
    f.extend_from_slice(&dst);
    f.extend_from_slice(&src);
    f.extend_from_slice(&ethertype.to_be_bytes());
    f.extend_from_slice(payload);
    if f.len() < 60 {
        f.resize(60, 0);
    }
    f
}

/// An ARP packet for IPv4 over Ethernet (RFC 826).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arp {
    /// 1, a request; 2, a reply.
    pub op: u16,
    pub sender_mac: Mac,
    pub sender_ip: Ipv4,
    pub target_mac: Mac,
    pub target_ip: Ipv4,
}

impl Arp {
    pub const REQUEST: u16 = 1;
    pub const REPLY: u16 = 2;

    pub fn parse(p: &[u8]) -> Result<Arp, Bad> {
        if p.len() < 28 {
            return Err(Bad::Short);
        }
        if p[0..6] != [0, 1, 0x08, 0x00, 6, 4] {
            return Err(Bad::Unsupported);
        }
        let op = be16(p, 6);
        if op != Self::REQUEST && op != Self::REPLY {
            return Err(Bad::Unsupported);
        }
        let take6 = |at: usize| -> Mac { p[at..at + 6].try_into().unwrap_or([0; 6]) };
        let take4 = |at: usize| -> Ipv4 { p[at..at + 4].try_into().unwrap_or([0; 4]) };
        Ok(Arp { op, sender_mac: take6(8), sender_ip: take4(14), target_mac: take6(18), target_ip: take4(24) })
    }

    pub fn bytes(&self) -> [u8; 28] {
        let mut b = [0u8; 28];
        b[0..6].copy_from_slice(&[0, 1, 0x08, 0x00, 6, 4]);
        b[6..8].copy_from_slice(&self.op.to_be_bytes());
        b[8..14].copy_from_slice(&self.sender_mac);
        b[14..18].copy_from_slice(&self.sender_ip);
        b[18..24].copy_from_slice(&self.target_mac);
        b[24..28].copy_from_slice(&self.target_ip);
        b
    }
}

/// An IPv4 packet: what netstack reads of its header, and its payload.
/// Options are skipped; fragments are refused (netstack doesn't reassemble
/// yet, and nothing it asks for comes fragmented).
#[derive(Debug, PartialEq, Eq)]
pub struct Ip<'a> {
    pub src: Ipv4,
    pub dst: Ipv4,
    pub proto: u8,
    pub ttl: u8,
    pub payload: &'a [u8],
}

impl<'a> Ip<'a> {
    pub fn parse(p: &'a [u8]) -> Result<Ip<'a>, Bad> {
        if p.len() < 20 {
            return Err(Bad::Short);
        }
        if p[0] >> 4 != 4 {
            return Err(Bad::Version);
        }
        let ihl = (p[0] & 0x0F) as usize * 4;
        if ihl < 20 || ihl > p.len() {
            return Err(Bad::HeaderLength);
        }
        let total = be16(p, 2) as usize;
        if total < ihl || total > p.len() {
            return Err(Bad::TotalLength);
        }
        if checksum(&p[..ihl], 0) != 0 {
            return Err(Bad::Checksum);
        }
        // More fragments, or an offset: a fragment.
        if be16(p, 6) & 0x3FFF != 0 {
            return Err(Bad::Fragment);
        }
        Ok(Ip {
            src: p[12..16].try_into().map_err(|_| Bad::Short)?,
            dst: p[16..20].try_into().map_err(|_| Bad::Short)?,
            proto: p[9],
            ttl: p[8],
            payload: &p[ihl..total],
        })
    }
}

/// An IPv4 packet from `src` to `dst` (no options, don't fragment, TTL 64).
pub fn ip(src: Ipv4, dst: Ipv4, proto: u8, id: u16, payload: &[u8]) -> Vec<u8> {
    let total = (20 + payload.len()) as u16;
    let mut h = [0u8; 20];
    h[0] = 0x45;
    h[2..4].copy_from_slice(&total.to_be_bytes());
    h[4..6].copy_from_slice(&id.to_be_bytes());
    h[6] = 0x40;
    h[8] = 64;
    h[9] = proto;
    h[12..16].copy_from_slice(&src);
    h[16..20].copy_from_slice(&dst);
    let sum = checksum(&h, 0);
    h[10..12].copy_from_slice(&sum.to_be_bytes());
    let mut p = Vec::with_capacity(total as usize);
    p.extend_from_slice(&h);
    p.extend_from_slice(payload);
    p
}

/// An ICMP echo request or reply (RFC 792).
#[derive(Debug, PartialEq, Eq)]
pub struct Echo<'a> {
    pub reply: bool,
    pub id: u16,
    pub seq: u16,
    pub data: &'a [u8],
}

impl<'a> Echo<'a> {
    /// An echo request or reply; anything else ICMP says is `Unsupported`.
    pub fn parse(p: &'a [u8]) -> Result<Echo<'a>, Bad> {
        if p.len() < 8 {
            return Err(Bad::Short);
        }
        if checksum(p, 0) != 0 {
            return Err(Bad::Checksum);
        }
        let reply = match (p[0], p[1]) {
            (0, 0) => true,
            (8, 0) => false,
            _ => return Err(Bad::Unsupported),
        };
        Ok(Echo { reply, id: be16(p, 4), seq: be16(p, 6), data: &p[8..] })
    }

    pub fn bytes(&self) -> Vec<u8> {
        let mut p = Vec::with_capacity(8 + self.data.len());
        p.extend_from_slice(&[if self.reply { 0 } else { 8 }, 0, 0, 0]);
        p.extend_from_slice(&self.id.to_be_bytes());
        p.extend_from_slice(&self.seq.to_be_bytes());
        p.extend_from_slice(self.data);
        let sum = checksum(&p, 0);
        p[2..4].copy_from_slice(&sum.to_be_bytes());
        p
    }
}

/// A UDP datagram (RFC 768), its checksum checked when it has one.
#[derive(Debug, PartialEq, Eq)]
pub struct Udp<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub data: &'a [u8],
}

impl<'a> Udp<'a> {
    pub fn parse(src: Ipv4, dst: Ipv4, p: &'a [u8]) -> Result<Udp<'a>, Bad> {
        if p.len() < 8 {
            return Err(Bad::Short);
        }
        let len = be16(p, 4) as usize;
        if len < 8 || len > p.len() {
            return Err(Bad::TotalLength);
        }
        let p = &p[..len];
        if be16(p, 6) != 0 && checksum(p, pseudo(src, dst, PROTO_UDP, len as u16)) != 0 {
            return Err(Bad::Checksum);
        }
        Ok(Udp { src_port: be16(p, 0), dst_port: be16(p, 2), data: &p[8..] })
    }
}

/// A UDP datagram, its checksum made (0 is sent as 0xFFFF).
pub fn udp(src: Ipv4, dst: Ipv4, src_port: u16, dst_port: u16, data: &[u8]) -> Vec<u8> {
    let len = (8 + data.len()) as u16;
    let mut p = Vec::with_capacity(len as usize);
    p.extend_from_slice(&src_port.to_be_bytes());
    p.extend_from_slice(&dst_port.to_be_bytes());
    p.extend_from_slice(&len.to_be_bytes());
    p.extend_from_slice(&[0, 0]);
    p.extend_from_slice(data);
    let sum = match checksum(&p, pseudo(src, dst, PROTO_UDP, len)) {
        0 => 0xFFFF,
        s => s,
    };
    p[6..8].copy_from_slice(&sum.to_be_bytes());
    p
}

#[cfg(test)]
mod tests;
