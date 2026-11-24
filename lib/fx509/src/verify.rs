// SPDX-License-Identifier: Apache-2.0
//! A server's chain checked (RFC 5280 § 6, as a TLS client needs it): each
//! certificate in date and signed by the next, the last by a root compiled
//! into the image; every issuer a CA allowed to sign certificates and within
//! its path length; the leaf's subjectAltName naming the host (the common
//! name is never used: RFC 9525), and allowed for server authentication.

use crate::cert::{Certificate, Key, SigAlg, Unreadable};

/// The most certificates a chain may hold, the leaf and the root's included.
pub const MAX_CHAIN: usize = 8;

/// Why a chain was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    Unreadable(Unreadable),
    Empty,
    TooLong,
    /// No root compiled in signed the last certificate.
    UnknownRoot,
    /// A certificate's signature doesn't verify with its issuer's key.
    Signature,
    Expired,
    NotYetValid,
    /// The leaf doesn't name this host.
    Name,
    /// An issuer that isn't a CA, may not sign certificates, or is past its path length.
    NotCa,
    /// The leaf may not be used for a server.
    Usage,
}

impl From<Unreadable> for Refused {
    fn from(u: Unreadable) -> Self {
        Refused::Unreadable(u)
    }
}

/// Whether `sig` over `msg` verifies with `key` under `alg`.
pub fn signed(key: &Key, alg: SigAlg, msg: &[u8], sig: &[u8]) -> bool {
    match (alg, key) {
        (SigAlg::RsaPkcs1(h), Key::Rsa { n, e }) => {
            fcrypto::rsa::PublicKey::new(n, e).is_some_and(|k| k.verify_pkcs1(h, &digest(h, msg)[..h.size()], sig))
        }
        (SigAlg::Ecdsa(h), Key::Ec { curve, x, y }) => ecdsa_rs(sig, curve.size()).is_some_and(|(r, s)| {
            fcrypto::ecdsa::verify(*curve, x, y, &digest(h, msg)[..h.size()], &r[..curve.size()], &s[..curve.size()])
        }),
        _ => false,
    }
}

fn digest(h: fcrypto::rsa::Hash, msg: &[u8]) -> [u8; 64] {
    let mut out = [0u8; 64];
    match h {
        fcrypto::rsa::Hash::Sha256 => out[..32].copy_from_slice(&fcrypto::sha2::sha256(msg)),
        fcrypto::rsa::Hash::Sha384 => out[..48].copy_from_slice(&fcrypto::sha2::sha384(msg)),
        fcrypto::rsa::Hash::Sha512 => out.copy_from_slice(&fcrypto::sha2::sha512(msg)),
    }
    out
}

/// An ECDSA signature's DER (SEQUENCE { r INTEGER, s INTEGER }) as two
/// big-endian numbers of `size` bytes each.
pub fn ecdsa_rs(sig: &[u8], size: usize) -> Option<([u8; 48], [u8; 48])> {
    let mut outer = crate::der::Reader::new(sig);
    let mut seq = outer.sequence().ok()?;
    outer.end().ok()?;
    let (r, s) = (seq.unsigned().ok()?, seq.unsigned().ok()?);
    seq.end().ok()?;
    if r.len() > size || s.len() > size {
        return None;
    }
    let (mut ro, mut so) = ([0u8; 48], [0u8; 48]);
    ro[size - r.len()..size].copy_from_slice(r);
    so[size - s.len()..size].copy_from_slice(s);
    Some((ro, so))
}

/// Whether a certificate's dNSName `pattern` names `host` (both ASCII,
/// compared without case; a `*` only as the whole leftmost label, standing
/// for exactly one label, and never for the last two).
pub fn name_matches(pattern: &[u8], host: &[u8]) -> bool {
    let eq = |a: &[u8], b: &[u8]| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y));
    let (pattern, host) = (pattern.strip_suffix(b".").unwrap_or(pattern), host.strip_suffix(b".").unwrap_or(host));
    if host.is_empty() || host.contains(&b'*') {
        return false;
    }
    match pattern.strip_prefix(b"*.") {
        Some(rest) => {
            // At least two labels after the wildcard (no *.com), none of them a wildcard.
            if rest.iter().filter(|&&c| c == b'.').count() < 1 || rest.contains(&b'*') {
                return false;
            }
            match host.iter().position(|&c| c == b'.') {
                Some(dot) if dot > 0 => eq(&host[dot + 1..], rest),
                _ => false,
            }
        }
        None => !pattern.contains(&b'*') && eq(pattern, host),
    }
}

/// Checks `chain` (DER, the leaf first) for `host` at `now` (Unix seconds)
/// against `roots` (DER), the only certificates trusted.
pub fn verify(chain: &[&[u8]], roots: &[&[u8]], host: &str, now: u64) -> Result<(), Refused> {
    if chain.is_empty() {
        return Err(Refused::Empty);
    }
    if chain.len() >= MAX_CHAIN {
        return Err(Refused::TooLong);
    }
    let leaf = Certificate::parse(chain[0])?;
    if !leaf.dns_names().any(|n| name_matches(n, host.as_bytes())) {
        return Err(Refused::Name);
    }
    if leaf.server_auth == Some(false) || leaf.key_usage.is_some_and(|(_, sign)| !sign) {
        return Err(Refused::Usage);
    }
    let mut below = leaf;
    // How many CA certificates lie under the one being checked (for pathLen).
    let mut cas_below = 0u64;
    for (i, der) in chain.iter().enumerate() {
        let c = if i == 0 { below.clone() } else { Certificate::parse(der)? };
        in_date(&c, now)?;
        if i > 0 {
            issued(&below, &c, cas_below)?;
            cas_below += 1;
        }
        below = c;
    }
    // The last one's issuer: a root compiled in (the last may be that root itself).
    for root in roots {
        let Ok(r) = Certificate::parse(root) else { continue };
        if r.subject == below.subject && r.key == below.key && r.tbs == below.tbs {
            return in_date(&r, now);
        }
        if r.subject == below.issuer && signed(&r.key, below.alg, below.tbs, below.signature) {
            in_date(&r, now)?;
            return ca(&r, cas_below);
        }
    }
    Err(Refused::UnknownRoot)
}

fn in_date(c: &Certificate, now: u64) -> Result<(), Refused> {
    if now < c.not_before {
        Err(Refused::NotYetValid)
    } else if now > c.not_after {
        Err(Refused::Expired)
    } else {
        Ok(())
    }
}

/// `issuer` may sign certificates with `cas_below` CA certificates under it.
fn ca(issuer: &Certificate, cas_below: u64) -> Result<(), Refused> {
    let may_sign = issuer.key_usage.is_none_or(|(cert_sign, _)| cert_sign);
    let path_ok = issuer.path_len.is_none_or(|p| cas_below <= p);
    if issuer.ca && may_sign && path_ok { Ok(()) } else { Err(Refused::NotCa) }
}

/// `child` was issued and signed by `issuer`.
fn issued(child: &Certificate, issuer: &Certificate, cas_below: u64) -> Result<(), Refused> {
    if child.issuer != issuer.subject {
        return Err(Refused::Signature);
    }
    ca(issuer, cas_below)?;
    if !signed(&issuer.key, child.alg, child.tbs, child.signature) {
        return Err(Refused::Signature);
    }
    Ok(())
}
