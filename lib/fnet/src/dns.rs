// SPDX-License-Identifier: Apache-2.0
//! DNS messages as a stub resolver sends and reads them (RFC 1035 § 4;
//! AAAA, RFC 3596): one question, recursion desired; answers of A, AAAA and
//! CNAME read, others skipped. Names: labels of at most 63 bytes, at most 255
//! in all, compression pointers that may only point back (so no loop), and
//! compared without case. Every length is checked before it's read.

use alloc::string::String;
use alloc::vec::Vec;

use crate::Bad;

pub const TYPE_A: u16 = 1;
pub const TYPE_CNAME: u16 = 5;
pub const TYPE_AAAA: u16 = 28;
pub const CLASS_IN: u16 = 1;
/// The DNS port.
pub const PORT: u16 = 53;
/// RCODE: the name doesn't exist.
pub const NXDOMAIN: u8 = 3;
/// The most CNAMEs followed for one name.
pub const MAX_ALIASES: usize = 8;

/// A record's data, as far as a resolver cares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Data {
    A([u8; 4]),
    Aaaa([u8; 16]),
    Cname(String),
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// Lower case, without the final dot.
    pub name: String,
    pub ttl: u32,
    pub data: Data,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub id: u16,
    /// TC: the answer didn't fit in UDP; ask again over TCP.
    pub truncated: bool,
    pub rcode: u8,
    pub question: (String, u16),
    pub answers: Vec<Record>,
}

/// A name in canonical form: lower case, no final dot; None if it can't be a
/// name (an empty label, one over 63 bytes, over 253 in all, or not ASCII).
pub fn canonical(name: &str) -> Option<String> {
    let n = name.strip_suffix('.').unwrap_or(name);
    let ok = !n.is_empty()
        && n.len() <= 253
        && n.is_ascii()
        && n.split('.').all(|l| !l.is_empty() && l.len() <= 63 && !l.contains(|c: char| c.is_ascii_whitespace()));
    ok.then(|| n.to_ascii_lowercase())
}

/// A query: `id`, recursion desired, one question for `name` of type `qtype`.
pub fn query(id: u16, name: &str, qtype: u16) -> Option<Vec<u8>> {
    let name = canonical(name)?;
    let mut m = Vec::with_capacity(18 + name.len());
    m.extend_from_slice(&id.to_be_bytes());
    m.extend_from_slice(&[0x01, 0x00]); // RD
    m.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]); // QDCOUNT 1
    for label in name.split('.') {
        m.push(label.len() as u8);
        m.extend_from_slice(label.as_bytes());
    }
    m.push(0);
    m.extend_from_slice(&qtype.to_be_bytes());
    m.extend_from_slice(&CLASS_IN.to_be_bytes());
    Some(m)
}

fn be16(m: &[u8], at: usize) -> Result<u16, Bad> {
    m.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]])).ok_or(Bad::Short)
}

fn be32(m: &[u8], at: usize) -> Result<u32, Bad> {
    m.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]])).ok_or(Bad::Short)
}

/// The name at `at`: (the name, where the field after it begins).
fn name(m: &[u8], mut at: usize) -> Result<(String, usize), Bad> {
    let mut out = String::new();
    let mut end = None;
    // A pointer must point before the place it's read from, so each jump goes
    // back and the walk ends.
    let mut limit = at;
    loop {
        let len = *m.get(at).ok_or(Bad::Short)? as usize;
        match len {
            0 => {
                let next = end.unwrap_or(at + 1);
                return Ok((out, next));
            }
            0xC0..=0xFF => {
                let target = (be16(m, at)? & 0x3FFF) as usize;
                if target >= limit {
                    return Err(Bad::Unsupported);
                }
                end.get_or_insert(at + 2);
                limit = target;
                at = target;
            }
            1..=63 => {
                let label = m.get(at + 1..at + 1 + len).ok_or(Bad::Short)?;
                if !label.is_ascii() {
                    return Err(Bad::Unsupported);
                }
                if !out.is_empty() {
                    out.push('.');
                }
                out.extend(label.iter().map(|&c| c.to_ascii_lowercase() as char));
                if out.len() > 253 {
                    return Err(Bad::TotalLength);
                }
                at += 1 + len;
            }
            _ => return Err(Bad::Unsupported), // 0x40 and 0x80: never used
        }
    }
}

/// Reads a response (QR set, a standard query's opcode).
pub fn parse(m: &[u8]) -> Result<Response, Bad> {
    if m.len() < 12 {
        return Err(Bad::Short);
    }
    let (flags, qd, an) = (be16(m, 2)?, be16(m, 4)?, be16(m, 6)?);
    if flags & 0x8000 == 0 || (flags >> 11) & 0xF != 0 || qd != 1 {
        return Err(Bad::Unsupported);
    }
    let (qname, mut at) = name(m, 12)?;
    let qtype = be16(m, at)?;
    if be16(m, at + 2)? != CLASS_IN {
        return Err(Bad::Unsupported);
    }
    at += 4;
    let mut answers = Vec::new();
    for _ in 0..an {
        let (rname, next) = name(m, at)?;
        let (rtype, class, ttl, len) = (be16(m, next)?, be16(m, next + 2)?, be32(m, next + 4)?, be16(m, next + 8)?);
        let start = next + 10;
        let rdata = m.get(start..start + len as usize).ok_or(Bad::Short)?;
        let data = match (rtype, class, rdata.len()) {
            (TYPE_A, CLASS_IN, 4) => Data::A(rdata.try_into().map_err(|_| Bad::Short)?),
            (TYPE_AAAA, CLASS_IN, 16) => Data::Aaaa(rdata.try_into().map_err(|_| Bad::Short)?),
            (TYPE_A | TYPE_AAAA, CLASS_IN, _) => return Err(Bad::TotalLength),
            (TYPE_CNAME, CLASS_IN, _) => {
                let (target, after) = name(m, start)?;
                if after != start + len as usize {
                    return Err(Bad::TotalLength);
                }
                Data::Cname(target)
            }
            _ => Data::Other,
        };
        // A TTL with the top bit set is 0 (RFC 2181 § 8).
        let ttl = if ttl & 0x8000_0000 != 0 { 0 } else { ttl };
        answers.push(Record { name: rname, ttl, data });
        at = start + len as usize;
    }
    Ok(Response {
        id: be16(m, 0)?,
        truncated: flags & 0x0200 != 0,
        rcode: (flags & 0xF) as u8,
        question: (qname, qtype),
        answers,
    })
}

/// What a response says of `name`'s addresses of type `qtype`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Found {
    /// The addresses (each A's 4 bytes or AAAA's 16), and the smallest TTL on the way.
    Addresses(Vec<Vec<u8>>, u32),
    /// The chain leaves the answers at this alias: ask again for it.
    Alias(String, u32),
    /// The name exists but has none of these.
    NoData,
    NoSuchName,
    /// Too many aliases, or a loop.
    TooDeep,
}

/// Follows `name` through the response's CNAMEs to its addresses.
pub fn found(r: &Response, name: &str, qtype: u16) -> Found {
    if r.rcode == NXDOMAIN {
        return Found::NoSuchName;
    }
    let mut at = String::from(name);
    let mut ttl = u32::MAX;
    for _ in 0..=MAX_ALIASES {
        let mut addrs = Vec::new();
        let mut alias = None;
        for rec in r.answers.iter().filter(|rec| rec.name == at) {
            match (&rec.data, qtype) {
                (Data::A(a), TYPE_A) => addrs.push((a.to_vec(), rec.ttl)),
                (Data::Aaaa(a), TYPE_AAAA) => addrs.push((a.to_vec(), rec.ttl)),
                (Data::Cname(t), _) => alias = Some((t.clone(), rec.ttl)),
                _ => {}
            }
        }
        if !addrs.is_empty() {
            let t = addrs.iter().map(|a| a.1).min().unwrap_or(0).min(ttl);
            return Found::Addresses(addrs.into_iter().map(|a| a.0).collect(), t);
        }
        match alias {
            Some((target, t)) => {
                ttl = ttl.min(t);
                at = target;
            }
            None if at == name => return Found::NoData,
            None => return Found::Alias(at, ttl),
        }
    }
    Found::TooDeep
}
