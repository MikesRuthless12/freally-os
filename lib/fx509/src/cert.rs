// SPDX-License-Identifier: Apache-2.0
//! An X.509 v3 certificate (RFC 5280 § 4.1), read without copying: what a
//! TLS client checks of it. Algorithms: RSA (PKCS#1 v1.5) and ECDSA (P-256,
//! P-384) with SHA-256/384/512; keys: RSA, EC on P-256 or P-384. Anything
//! else is refused, never guessed at.

use crate::der::{Bad, Reader, tag};

/// How the issuer signed: the scheme and its hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SigAlg {
    RsaPkcs1(fcrypto::rsa::Hash),
    Ecdsa(fcrypto::rsa::Hash),
}

/// The subject's public key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key<'a> {
    Rsa {
        n: &'a [u8],
        e: &'a [u8],
    },
    /// An uncompressed point's coordinates.
    Ec {
        curve: fcrypto::ecdsa::Curve,
        x: &'a [u8],
        y: &'a [u8],
    },
}

#[derive(Clone, Debug)]
pub struct Certificate<'a> {
    /// The signed part, whole (what the signature covers).
    pub tbs: &'a [u8],
    pub alg: SigAlg,
    pub signature: &'a [u8],
    /// The issuer's and subject's names, as their DER (compared byte for byte).
    pub issuer: &'a [u8],
    pub subject: &'a [u8],
    pub not_before: u64,
    pub not_after: u64,
    pub key: Key<'a>,
    /// subjectAltName's names (the GeneralNames' contents, checked well formed).
    san: &'a [u8],
    /// basicConstraints: a CA, and its path length limit.
    pub ca: bool,
    pub path_len: Option<u64>,
    /// keyUsage, when present: whether keyCertSign (bit 5) and digitalSignature (bit 0) are set.
    pub key_usage: Option<(bool, bool)>,
    /// extKeyUsage, when present: whether it allows serverAuth.
    pub server_auth: Option<bool>,
}

const SHA256_RSA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0B];
const SHA384_RSA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0C];
const SHA512_RSA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0D];
const ECDSA_SHA256: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x02];
const ECDSA_SHA384: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x03];
const ECDSA_SHA512: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x04];
const RSA_KEY: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x01];
const EC_KEY: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01];
const P256: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07];
const P384: &[u8] = &[0x2B, 0x81, 0x04, 0x00, 0x22];
const SAN: &[u8] = &[0x55, 0x1D, 0x11];
const BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1D, 0x13];
const KEY_USAGE: &[u8] = &[0x55, 0x1D, 0x0F];
const EXT_KEY_USAGE: &[u8] = &[0x55, 0x1D, 0x25];
const SERVER_AUTH: &[u8] = &[0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];
const ANY_EKU: &[u8] = &[0x55, 0x1D, 0x25, 0x00];

/// Why a certificate couldn't be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unreadable {
    Der(Bad),
    /// Not version 3.
    Version,
    /// A signature or key algorithm (or curve) not taken.
    Algorithm,
    /// A critical extension this reader doesn't know: RFC 5280 says refuse.
    Critical,
}

impl From<Bad> for Unreadable {
    fn from(b: Bad) -> Self {
        Unreadable::Der(b)
    }
}

fn sig_alg(r: &mut Reader) -> Result<SigAlg, Unreadable> {
    use fcrypto::rsa::Hash::*;
    let mut a = r.sequence()?;
    let oid = a.read(tag::OID)?;
    let alg = match oid {
        SHA256_RSA => SigAlg::RsaPkcs1(Sha256),
        SHA384_RSA => SigAlg::RsaPkcs1(Sha384),
        SHA512_RSA => SigAlg::RsaPkcs1(Sha512),
        ECDSA_SHA256 => SigAlg::Ecdsa(Sha256),
        ECDSA_SHA384 => SigAlg::Ecdsa(Sha384),
        ECDSA_SHA512 => SigAlg::Ecdsa(Sha512),
        _ => return Err(Unreadable::Algorithm),
    };
    // RSA's parameters are NULL (present or absent); ECDSA's are absent.
    if matches!(alg, SigAlg::RsaPkcs1(_)) {
        a.optional(tag::NULL)?;
    }
    a.end()?;
    Ok(alg)
}

fn key<'a>(r: &mut Reader<'a>) -> Result<Key<'a>, Unreadable> {
    let mut spki = r.sequence()?;
    let mut a = spki.sequence()?;
    let oid = a.read(tag::OID)?;
    let bits = spki.bits()?;
    spki.end()?;
    match oid {
        RSA_KEY => {
            a.optional(tag::NULL)?;
            a.end()?;
            let mut k = Reader::new(bits).sequence()?;
            let (n, e) = (k.unsigned()?, k.unsigned()?);
            k.end()?;
            Ok(Key::Rsa { n, e })
        }
        EC_KEY => {
            let curve = match a.read(tag::OID)? {
                P256 => fcrypto::ecdsa::Curve::P256,
                P384 => fcrypto::ecdsa::Curve::P384,
                _ => return Err(Unreadable::Algorithm),
            };
            a.end()?;
            let size = curve.size();
            match bits {
                [4, rest @ ..] if rest.len() == 2 * size => Ok(Key::Ec { curve, x: &rest[..size], y: &rest[size..] }),
                _ => Err(Unreadable::Algorithm),
            }
        }
        _ => Err(Unreadable::Algorithm),
    }
}

impl<'a> Certificate<'a> {
    /// Reads one certificate's DER, whole (nothing may follow it).
    pub fn parse(der: &'a [u8]) -> Result<Certificate<'a>, Unreadable> {
        let mut outer = Reader::new(der);
        let mut cert = outer.sequence()?;
        outer.end()?;
        let (_, tbs_body, tbs) = cert.any()?;
        let alg = sig_alg(&mut cert)?;
        let signature = cert.bits()?;
        cert.end()?;
        let mut t = Reader::new(tbs_body);
        // version [0] EXPLICIT: v3 (2) only.
        let mut v = Reader::new(t.read(tag::explicit(0)).map_err(|_| Unreadable::Version)?);
        if v.small()? != 2 {
            return Err(Unreadable::Version);
        }
        t.unsigned()?; // the serial number
        if sig_alg(&mut t)? != alg {
            return Err(Unreadable::Algorithm); // RFC 5280 § 4.1.1.2: the two must match
        }
        let (_, _, issuer) = t.any()?;
        let mut validity = t.sequence()?;
        let (not_before, not_after) = (validity.time()?, validity.time()?);
        validity.end()?;
        let (_, _, subject) = t.any()?;
        let key = key(&mut t)?;
        let mut c = Certificate {
            tbs,
            alg,
            signature,
            issuer,
            subject,
            not_before,
            not_after,
            key,
            san: &[],
            ca: false,
            path_len: None,
            key_usage: None,
            server_auth: None,
        };
        t.optional(tag::implicit(1))?;
        t.optional(tag::implicit(2))?;
        if let Some(ext) = t.optional(tag::explicit(3))? {
            let mut list = Reader::new(ext).sequence()?;
            while !list.is_empty() {
                let mut e = list.sequence()?;
                let oid = e.read(tag::OID)?;
                let critical = e.optional(tag::BOOLEAN)?.is_some_and(|b| b == [0xFF]);
                let value = e.read(tag::OCTET_STRING)?;
                e.end()?;
                if !c.extension(oid, value)? && critical {
                    return Err(Unreadable::Critical);
                }
            }
        }
        t.end()?;
        Ok(c)
    }

    /// One extension read; whether it's one this reader knows.
    fn extension(&mut self, oid: &[u8], value: &'a [u8]) -> Result<bool, Unreadable> {
        let mut r = Reader::new(value);
        match oid {
            SAN => {
                let names = r.sequence()?;
                let mut check = names;
                while !check.is_empty() {
                    check.any()?;
                }
                self.san = value;
            }
            BASIC_CONSTRAINTS => {
                let mut s = r.sequence()?;
                self.ca = s.optional(tag::BOOLEAN)?.is_some_and(|b| b == [0xFF]);
                if s.peek() == Some(tag::INTEGER) {
                    self.path_len = Some(s.small()?);
                }
                s.end()?;
            }
            KEY_USAGE => {
                let b = r.read(tag::BIT_STRING)?;
                let first = b.get(1).copied().unwrap_or(0);
                self.key_usage = Some((first & 0x04 != 0, first & 0x80 != 0));
            }
            EXT_KEY_USAGE => {
                let mut s = r.sequence()?;
                let mut ok = false;
                while !s.is_empty() {
                    let p = s.read(tag::OID)?;
                    ok |= p == SERVER_AUTH || p == ANY_EKU;
                }
                self.server_auth = Some(ok);
            }
            _ => return Ok(false),
        }
        r.end()?;
        Ok(true)
    }

    /// subjectAltName's dNSNames ([2] IMPLICIT IA5String), as written; other kinds skipped.
    pub fn dns_names(&self) -> impl Iterator<Item = &'a [u8]> {
        let mut names = Reader::new(self.san).sequence().unwrap_or(Reader::new(&[]));
        core::iter::from_fn(move || {
            while !names.is_empty() {
                match names.any() {
                    Ok((t, v, _)) if t == tag::implicit(2) => return Some(v),
                    Ok(_) => {}
                    Err(_) => return None,
                }
            }
            None
        })
    }
}
