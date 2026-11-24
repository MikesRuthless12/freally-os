// SPDX-License-Identifier: Apache-2.0
//! TCP segments (RFC 9293 § 3.1): the header; the options a client uses, MSS
//! (§ 3.7.1) and window scale (RFC 7323 § 2), with every other option's
//! length checked and the option skipped; the checksum over the pseudo-header
//! of either IP version, passed in as its sum ([`pseudo4`], [`pseudo6`]).

use alloc::vec::Vec;

use crate::ipv6::Ipv6;
use crate::{Bad, Ipv4, be16, checksum};

pub const PROTO_TCP: u8 = 6;
/// The largest shift a window scale may ask for (RFC 7323 § 2.3).
pub const MAX_WSCALE: u8 = 14;

pub mod flag {
    pub const FIN: u8 = 0x01;
    pub const SYN: u8 = 0x02;
    pub const RST: u8 = 0x04;
    pub const PSH: u8 = 0x08;
    pub const ACK: u8 = 0x10;
}

/// The IPv4 pseudo-header's sum for a segment of `len` bytes.
pub fn pseudo4(src: Ipv4, dst: Ipv4, len: usize) -> u32 {
    crate::pseudo(src, dst, PROTO_TCP, len as u16)
}

/// The IPv6 pseudo-header's sum for a segment of `len` bytes.
pub fn pseudo6(src: &Ipv6, dst: &Ipv6, len: usize) -> u32 {
    crate::ipv6::pseudo6(src, dst, PROTO_TCP, len as u32)
}

/// A segment, read or to be written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tcp<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    pub ack: u32,
    pub flags: u8,
    pub window: u16,
    pub mss: Option<u16>,
    /// The window scale's shift, at most 14 (a larger one is taken as 14).
    pub wscale: Option<u8>,
    pub data: &'a [u8],
}

impl<'a> Tcp<'a> {
    /// Reads a segment whose pseudo-header sums to `pseudo`.
    pub fn parse(p: &'a [u8], pseudo: u32) -> Result<Tcp<'a>, Bad> {
        if p.len() < 20 {
            return Err(Bad::Short);
        }
        let header = (p[12] >> 4) as usize * 4;
        if header < 20 || header > p.len() {
            return Err(Bad::HeaderLength);
        }
        if checksum(p, pseudo) != 0 {
            return Err(Bad::Checksum);
        }
        let mut t = Tcp {
            src_port: be16(p, 0),
            dst_port: be16(p, 2),
            seq: u32::from_be_bytes([p[4], p[5], p[6], p[7]]),
            ack: u32::from_be_bytes([p[8], p[9], p[10], p[11]]),
            flags: p[13],
            window: be16(p, 14),
            mss: None,
            wscale: None,
            data: &p[header..],
        };
        let mut o = &p[20..header];
        while let [kind, rest @ ..] = o {
            match kind {
                0 => break,
                1 => o = rest,
                _ => {
                    let len = *rest.first().ok_or(Bad::HeaderLength)? as usize;
                    if len < 2 || len > o.len() {
                        return Err(Bad::HeaderLength);
                    }
                    match (kind, len) {
                        (2, 4) => t.mss = Some(be16(o, 2)),
                        (3, 3) => t.wscale = Some(o[2].min(MAX_WSCALE)),
                        (2 | 3, _) => return Err(Bad::HeaderLength),
                        _ => {}
                    }
                    o = &o[len..];
                }
            }
        }
        Ok(t)
    }

    /// The segment's bytes, its checksum made over `pseudo` (the pseudo-header
    /// of this length: [`Tcp::wire_len`]).
    pub fn bytes(&self, pseudo: u32) -> Vec<u8> {
        let options = if self.mss.is_some() { 4 } else { 0 } + if self.wscale.is_some() { 4 } else { 0 };
        let header = 20 + options;
        let mut p = Vec::with_capacity(header + self.data.len());
        p.extend_from_slice(&self.src_port.to_be_bytes());
        p.extend_from_slice(&self.dst_port.to_be_bytes());
        p.extend_from_slice(&self.seq.to_be_bytes());
        p.extend_from_slice(&self.ack.to_be_bytes());
        p.push(((header / 4) as u8) << 4);
        p.push(self.flags);
        p.extend_from_slice(&self.window.to_be_bytes());
        p.extend_from_slice(&[0, 0, 0, 0]); // the checksum, the urgent pointer
        if let Some(m) = self.mss {
            p.extend_from_slice(&[2, 4]);
            p.extend_from_slice(&m.to_be_bytes());
        }
        if let Some(s) = self.wscale {
            p.extend_from_slice(&[1, 3, 3, s.min(MAX_WSCALE)]);
        }
        p.extend_from_slice(self.data);
        let sum = checksum(&p, pseudo);
        p[16..18].copy_from_slice(&sum.to_be_bytes());
        p
    }

    /// The segment's length on the wire.
    pub fn wire_len(&self) -> usize {
        20 + if self.mss.is_some() { 4 } else { 0 } + if self.wscale.is_some() { 4 } else { 0 } + self.data.len()
    }

    /// How much sequence space it takes: its data, and one each for SYN and FIN.
    pub fn seq_len(&self) -> u32 {
        self.data.len() as u32 + (self.flags & flag::SYN != 0) as u32 + (self.flags & flag::FIN != 0) as u32
    }
}
