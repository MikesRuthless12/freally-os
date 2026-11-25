// SPDX-License-Identifier: Apache-2.0
//! IPv6 (RFC 8200), ICMPv6 (RFC 4443) and Neighbor Discovery (RFC 4861):
//! router and neighbour solicitations and advertisements, their link-layer
//! address, prefix (RFC 4862's SLAAC) and RDNSS (RFC 8106) options. Every
//! length is checked before it's read; extension headers aren't followed
//! (a packet with one is refused: nothing netstack asks for carries one).

use crate::{Bad, Mac, checksum};
use alloc::vec::Vec;

pub type Ipv6 = [u8; 16];

pub const ETHERTYPE_IPV6: u16 = 0x86DD;
pub const NEXT_ICMPV6: u8 = 58;
pub const NEXT_UDP: u8 = 17;
pub const UNSPECIFIED: Ipv6 = [0; 16];
/// ff02::2, every router on the link.
pub const ALL_ROUTERS: Ipv6 = [0xFF, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2];

/// ICMPv6 types netstack speaks.
pub mod kind {
    pub const ECHO_REQUEST: u8 = 128;
    pub const ECHO_REPLY: u8 = 129;
    pub const ROUTER_SOLICITATION: u8 = 133;
    pub const ROUTER_ADVERTISEMENT: u8 = 134;
    pub const NEIGHBOR_SOLICITATION: u8 = 135;
    pub const NEIGHBOR_ADVERTISEMENT: u8 = 136;
}

/// An IPv6 packet: its addresses, the next header, the hop limit, the payload.
#[derive(Debug, PartialEq, Eq)]
pub struct Ip6<'a> {
    pub src: Ipv6,
    pub dst: Ipv6,
    pub next: u8,
    pub hops: u8,
    pub payload: &'a [u8],
}

impl<'a> Ip6<'a> {
    pub fn parse(p: &'a [u8]) -> Result<Ip6<'a>, Bad> {
        if p.len() < 40 {
            return Err(Bad::Short);
        }
        if p[0] >> 4 != 6 {
            return Err(Bad::Version);
        }
        let len = u16::from_be_bytes([p[4], p[5]]) as usize;
        if 40 + len > p.len() {
            return Err(Bad::TotalLength);
        }
        Ok(Ip6 {
            src: p[8..24].try_into().map_err(|_| Bad::Short)?,
            dst: p[24..40].try_into().map_err(|_| Bad::Short)?,
            next: p[6],
            hops: p[7],
            payload: &p[40..40 + len],
        })
    }
}

/// An IPv6 packet from `src` to `dst` with `hops` hops left.
pub fn ip6(src: Ipv6, dst: Ipv6, next: u8, hops: u8, payload: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(40 + payload.len());
    p.extend_from_slice(&[0x60, 0, 0, 0]);
    p.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    p.extend_from_slice(&[next, hops]);
    p.extend_from_slice(&src);
    p.extend_from_slice(&dst);
    p.extend_from_slice(payload);
    p
}

/// The IPv6 pseudo-header's sum (RFC 8200 § 8.1).
pub(crate) fn pseudo6(src: &Ipv6, dst: &Ipv6, next: u8, len: u32) -> u32 {
    let words = |a: &Ipv6| a.as_chunks::<2>().0.iter().map(|w| u16::from_be_bytes(*w) as u32).sum::<u32>();
    words(src) + words(dst) + (len >> 16) + (len & 0xFFFF) + next as u32
}

/// An ICMPv6 message's bytes with its checksum made.
pub fn icmp6(src: &Ipv6, dst: &Ipv6, kind: u8, code: u8, body: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(4 + body.len());
    m.extend_from_slice(&[kind, code, 0, 0]);
    m.extend_from_slice(body);
    let sum = checksum(&m, pseudo6(src, dst, NEXT_ICMPV6, m.len() as u32));
    m[2..4].copy_from_slice(&sum.to_be_bytes());
    m
}

/// An ICMPv6 message read: its type, code and body, its checksum checked.
pub fn icmp6_parse<'a>(src: &Ipv6, dst: &Ipv6, m: &'a [u8]) -> Result<(u8, u8, &'a [u8]), Bad> {
    if m.len() < 4 {
        return Err(Bad::Short);
    }
    if checksum(m, pseudo6(src, dst, NEXT_ICMPV6, m.len() as u32)) != 0 {
        return Err(Bad::Checksum);
    }
    Ok((m[0], m[1], &m[4..]))
}

/// A UDP datagram over IPv6 (TASK-176), its checksum made over the IPv6
/// pseudo-header (0 is sent as 0xFFFF: over IPv6 a checksum is required).
pub fn udp6(src: &Ipv6, dst: &Ipv6, src_port: u16, dst_port: u16, data: &[u8]) -> Vec<u8> {
    let len = 8 + data.len();
    let mut p = Vec::with_capacity(len);
    p.extend_from_slice(&src_port.to_be_bytes());
    p.extend_from_slice(&dst_port.to_be_bytes());
    p.extend_from_slice(&(len as u16).to_be_bytes());
    p.extend_from_slice(&[0, 0]);
    p.extend_from_slice(data);
    let sum = match checksum(&p, pseudo6(src, dst, NEXT_UDP, len as u32)) {
        0 => 0xFFFF,
        s => s,
    };
    p[6..8].copy_from_slice(&sum.to_be_bytes());
    p
}

/// A UDP datagram over IPv6 read, its checksum checked; one with none (0)
/// is refused (RFC 8200 § 8.1).
pub fn udp6_parse<'a>(src: &Ipv6, dst: &Ipv6, p: &'a [u8]) -> Result<crate::Udp<'a>, Bad> {
    if p.len() < 8 {
        return Err(Bad::Short);
    }
    let len = u16::from_be_bytes([p[4], p[5]]) as usize;
    if len < 8 || len > p.len() {
        return Err(Bad::TotalLength);
    }
    let p = &p[..len];
    if p[6..8] == [0, 0] || checksum(p, pseudo6(src, dst, NEXT_UDP, len as u32)) != 0 {
        return Err(Bad::Checksum);
    }
    let port = |at: usize| u16::from_be_bytes([p[at], p[at + 1]]);
    Ok(crate::Udp { src_port: port(0), dst_port: port(2), data: &p[8..] })
}

/// The solicited-node multicast address of `a` (ff02::1:ffXX:XXXX).
pub fn solicited_node(a: &Ipv6) -> Ipv6 {
    let mut s = [0xFF, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0xFF, 0, 0, 0];
    s[13..].copy_from_slice(&a[13..]);
    s
}

/// The Ethernet multicast address of an IPv6 multicast address (33:33 and its last four bytes).
pub fn multicast_mac(a: &Ipv6) -> Mac {
    [0x33, 0x33, a[12], a[13], a[14], a[15]]
}

/// fe80::/64 with this interface id.
pub fn link_local(iid: [u8; 8]) -> Ipv6 {
    let mut a = [0u8; 16];
    a[0] = 0xFE;
    a[1] = 0x80;
    a[8..].copy_from_slice(&iid);
    a
}

/// What a router advertised (RFC 4861 § 4.2): the prefixes to make
/// addresses from (on-link, autonomous, /64), its lifetime, the DNS servers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RouterAd {
    pub router_lifetime_s: u16,
    pub mac: Option<Mac>,
    pub prefixes: Vec<(Ipv6, u8, u32)>,
    pub dns: Vec<Ipv6>,
}

/// A router advertisement's body (after the ICMPv6 header): its options read.
pub fn router_ad(body: &[u8]) -> Result<RouterAd, Bad> {
    if body.len() < 12 {
        return Err(Bad::Short);
    }
    let mut ad = RouterAd { router_lifetime_s: u16::from_be_bytes([body[2], body[3]]), ..RouterAd::default() };
    let mut o = &body[12..];
    while !o.is_empty() {
        if o.len() < 2 {
            return Err(Bad::Short);
        }
        let len = o[1] as usize * 8;
        if len == 0 || len > o.len() {
            return Err(Bad::Short);
        }
        let opt = &o[..len];
        match opt[0] {
            1 if len >= 8 => ad.mac = opt[2..8].try_into().ok(),
            // Prefix information: length, flags (L on-link, A autonomous), valid lifetime, prefix.
            3 if len == 32 => {
                let (plen, flags) = (opt[2], opt[3]);
                let valid = u32::from_be_bytes([opt[4], opt[5], opt[6], opt[7]]);
                if flags & 0x40 != 0 && plen == 64 && valid > 0 {
                    let prefix: Ipv6 = opt[16..32].try_into().map_err(|_| Bad::Short)?;
                    ad.prefixes.push((prefix, plen, valid));
                }
            }
            // RDNSS: reserved, lifetime, then the servers.
            25 if len >= 24 => {
                ad.dns.extend_from_slice(opt[8..].as_chunks::<16>().0);
            }
            _ => {}
        }
        o = &o[len..];
    }
    Ok(ad)
}

/// A router solicitation's body, with our link-layer address when we have an address.
pub fn router_solicitation(mac: Option<Mac>) -> Vec<u8> {
    let mut b = alloc::vec![0u8; 4];
    if let Some(m) = mac {
        b.extend_from_slice(&[1, 1]);
        b.extend_from_slice(&m);
    }
    b
}

/// A neighbour solicitation's body for `target`, with our link-layer address
/// (left out when asking from the unspecified address, for DAD).
pub fn neighbor_solicitation(target: &Ipv6, mac: Option<Mac>) -> Vec<u8> {
    let mut b = alloc::vec![0u8; 4];
    b.extend_from_slice(target);
    if let Some(m) = mac {
        b.extend_from_slice(&[1, 1]);
        b.extend_from_slice(&m);
    }
    b
}

/// A neighbour advertisement's body: solicited (S) and override (O), the
/// target, our link-layer address.
pub fn neighbor_advertisement(target: &Ipv6, mac: Mac, solicited: bool) -> Vec<u8> {
    let mut b = alloc::vec![if solicited { 0x60 } else { 0x20 }, 0, 0, 0];
    b.extend_from_slice(target);
    b.extend_from_slice(&[2, 1]);
    b.extend_from_slice(&mac);
    b
}

/// A neighbour solicitation's or advertisement's target, and the link-layer
/// address its option gives (source for a solicitation, target for an advertisement).
pub fn neighbor(body: &[u8]) -> Result<(Ipv6, Option<Mac>), Bad> {
    if body.len() < 20 {
        return Err(Bad::Short);
    }
    let target: Ipv6 = body[4..20].try_into().map_err(|_| Bad::Short)?;
    let mut mac = None;
    let mut o = &body[20..];
    while o.len() >= 2 {
        let len = o[1] as usize * 8;
        if len == 0 || len > o.len() {
            return Err(Bad::Short);
        }
        if (o[0] == 1 || o[0] == 2) && len >= 8 {
            mac = o[2..8].try_into().ok();
        }
        o = &o[len..];
    }
    Ok((target, mac))
}

/// An address as RFC 5952 writes it: lowercase, no leading zeros, the
/// longest run of two or more zero groups (the first, on a tie) as `::`.
pub fn text(a: &Ipv6) -> alloc::string::String {
    use core::fmt::Write;
    let g: [u16; 8] = core::array::from_fn(|i| u16::from_be_bytes([a[2 * i], a[2 * i + 1]]));
    let (mut best, mut run) = ((0usize, 0usize), (0usize, 0usize));
    for (i, &w) in g.iter().enumerate() {
        if w == 0 {
            run = if run.1 == 0 { (i, 1) } else { (run.0, run.1 + 1) };
            if run.1 > best.1 {
                best = run;
            }
        } else {
            run = (0, 0);
        }
    }
    let mut s = alloc::string::String::new();
    let mut i = 0;
    while i < 8 {
        if best.1 >= 2 && i == best.0 {
            s.push_str("::");
            i += best.1;
            continue;
        }
        if !s.is_empty() && !s.ends_with(':') {
            s.push(':');
        }
        let _ = write!(s, "{:x}", g[i]);
        i += 1;
    }
    s
}

/// An address read from text: eight groups, or fewer around one `::`.
pub fn parse_text(s: &str) -> Option<Ipv6> {
    let groups = |part: &str| -> Option<alloc::vec::Vec<u16>> {
        if part.is_empty() {
            return Some(alloc::vec::Vec::new());
        }
        part.split(':')
            .map(|h| {
                (!h.is_empty() && h.len() <= 4 && h.bytes().all(|c| c.is_ascii_hexdigit()))
                    .then(|| u16::from_str_radix(h, 16).ok())
                    .flatten()
            })
            .collect()
    };
    let words: alloc::vec::Vec<u16> = match s.split_once("::") {
        Some((head, tail)) => {
            let (h, t) = (groups(head)?, groups(tail)?);
            if h.len() + t.len() > 7 || tail.contains("::") {
                return None;
            }
            let mut w = h;
            w.resize(8 - t.len(), 0);
            w.extend(t);
            w
        }
        None => groups(s)?,
    };
    if words.len() != 8 {
        return None;
    }
    let mut a = [0u8; 16];
    for (i, w) in words.iter().enumerate() {
        a[2 * i..2 * i + 2].copy_from_slice(&w.to_be_bytes());
    }
    Some(a)
}
