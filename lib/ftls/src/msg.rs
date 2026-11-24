// SPDX-License-Identifier: Apache-2.0
//! TLS 1.3's handshake messages as a client writes and reads them (RFC 8446
//! § 4): every length checked before a byte is read, every extension the
//! client didn't offer refused.

use alloc::vec::Vec;

use crate::Alert;

pub mod hs {
    pub const CLIENT_HELLO: u8 = 1;
    pub const SERVER_HELLO: u8 = 2;
    pub const NEW_SESSION_TICKET: u8 = 4;
    pub const ENCRYPTED_EXTENSIONS: u8 = 8;
    pub const CERTIFICATE: u8 = 11;
    pub const CERTIFICATE_REQUEST: u8 = 13;
    pub const CERTIFICATE_VERIFY: u8 = 15;
    pub const FINISHED: u8 = 20;
    pub const KEY_UPDATE: u8 = 24;
}

pub mod ext {
    pub const SERVER_NAME: u16 = 0;
    pub const SUPPORTED_GROUPS: u16 = 10;
    pub const SIGNATURE_ALGORITHMS: u16 = 13;
    pub const SUPPORTED_VERSIONS: u16 = 43;
    pub const COOKIE: u16 = 44;
    pub const KEY_SHARE: u16 = 51;
}

pub mod group {
    pub const X25519: u16 = 0x001D;
    pub const SECP256R1: u16 = 0x0017;
    /// Offered, in order: X25519's share is sent at once; P-256's only when a
    /// HelloRetryRequest asks for it.
    pub const OFFERED: [u16; 2] = [X25519, SECP256R1];
}

pub mod scheme {
    pub const ECDSA_P256_SHA256: u16 = 0x0403;
    pub const ECDSA_P384_SHA384: u16 = 0x0503;
    pub const RSA_PSS_SHA256: u16 = 0x0804;
    pub const RSA_PSS_SHA384: u16 = 0x0805;
    pub const RSA_PSS_SHA512: u16 = 0x0806;
    pub const RSA_PKCS1_SHA256: u16 = 0x0401;
    pub const RSA_PKCS1_SHA384: u16 = 0x0501;
    pub const RSA_PKCS1_SHA512: u16 = 0x0601;
    /// Offered: the handshake's schemes, then PKCS#1 v1.5, which TLS 1.3
    /// allows in certificates only (§ 4.2.3).
    pub const OFFERED: [u16; 8] = [
        ECDSA_P256_SHA256,
        ECDSA_P384_SHA384,
        RSA_PSS_SHA256,
        RSA_PSS_SHA384,
        RSA_PSS_SHA512,
        RSA_PKCS1_SHA256,
        RSA_PKCS1_SHA384,
        RSA_PKCS1_SHA512,
    ];
}

pub const TLS13: u16 = 0x0304;
/// A HelloRetryRequest is a ServerHello with this random: SHA-256("HelloRetryRequest").
pub const RETRY_RANDOM: [u8; 32] = [
    0xCF, 0x21, 0xAD, 0x74, 0xE5, 0x9A, 0x61, 0x11, 0xBE, 0x1D, 0x8C, 0x02, 0x1E, 0x65, 0xB8, 0x91, 0xC2, 0xA2, 0x11,
    0x16, 0x7A, 0xBB, 0x8C, 0x5E, 0x07, 0x9E, 0x09, 0xE2, 0xC8, 0xA8, 0x33, 0x9C,
];

/// Reads a message's fields; a read past the end is a decode_error.
#[derive(Clone, Copy)]
pub struct Rd<'a> {
    b: &'a [u8],
}

impl<'a> Rd<'a> {
    pub fn new(b: &'a [u8]) -> Rd<'a> {
        Rd { b }
    }

    pub fn is_empty(&self) -> bool {
        self.b.is_empty()
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], Alert> {
        if self.b.len() < n {
            return Err(Alert::DECODE_ERROR);
        }
        let (a, rest) = self.b.split_at(n);
        self.b = rest;
        Ok(a)
    }

    pub fn u8(&mut self) -> Result<u8, Alert> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, Alert> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn u24(&mut self) -> Result<usize, Alert> {
        let b = self.take(3)?;
        Ok((b[0] as usize) << 16 | (b[1] as usize) << 8 | b[2] as usize)
    }

    /// A vector with a length of `n` bytes before it.
    pub fn vec(&mut self, n: usize) -> Result<&'a [u8], Alert> {
        let len = match n {
            1 => self.u8()? as usize,
            2 => self.u16()? as usize,
            _ => self.u24()?,
        };
        self.take(len)
    }

    pub fn end(&self) -> Result<(), Alert> {
        if self.b.is_empty() { Ok(()) } else { Err(Alert::DECODE_ERROR) }
    }
}

/// Writes what `f` writes as a vector with an `n`-byte length before it.
fn vec(v: &mut Vec<u8>, n: usize, f: impl FnOnce(&mut Vec<u8>)) {
    let at = v.len();
    v.resize(at + n, 0);
    f(v);
    let len = v.len() - at - n;
    v[at..at + n].copy_from_slice(&len.to_be_bytes()[size_of::<usize>() - n..]);
}

fn u16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_be_bytes());
}

/// A handshake message: its type, a 3-byte length, its body.
pub fn message(ty: u8, f: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut m = alloc::vec![ty];
    vec(&mut m, 3, f);
    m
}

/// Whether `host` is an IP address literal, which never goes in server_name (RFC 6066 § 3).
fn is_ip_literal(host: &str) -> bool {
    host.contains(':') || (host.split('.').count() == 4 && host.split('.').all(|p| p.parse::<u8>().is_ok()))
}

/// The ClientHello: `shares` are (group, key_exchange) for the key_share
/// extension; `groups` are every group the client takes; `cookie` echoes a
/// HelloRetryRequest's.
pub fn client_hello(
    random: &[u8; 32],
    host: &str,
    shares: &[(u16, &[u8])],
    groups: &[u16],
    cookie: Option<&[u8]>,
) -> Vec<u8> {
    message(hs::CLIENT_HELLO, |m| {
        u16(m, 0x0303); // legacy_version
        m.extend_from_slice(random);
        m.push(0); // legacy_session_id: empty (no middlebox compatibility mode)
        vec(m, 2, |m| Suite::OFFERED.iter().for_each(|s| u16(m, s.id())));
        m.extend_from_slice(&[1, 0]); // legacy_compression_methods: null
        vec(m, 2, |m| {
            if !is_ip_literal(host) {
                u16(m, ext::SERVER_NAME);
                vec(m, 2, |m| {
                    vec(m, 2, |m| {
                        m.push(0); // host_name
                        vec(m, 2, |m| m.extend_from_slice(host.as_bytes()));
                    })
                });
            }
            u16(m, ext::SUPPORTED_GROUPS);
            vec(m, 2, |m| vec(m, 2, |m| groups.iter().for_each(|&g| u16(m, g))));
            u16(m, ext::SIGNATURE_ALGORITHMS);
            vec(m, 2, |m| vec(m, 2, |m| scheme::OFFERED.iter().for_each(|&s| u16(m, s))));
            u16(m, ext::SUPPORTED_VERSIONS);
            vec(m, 2, |m| vec(m, 1, |m| u16(m, TLS13)));
            if let Some(c) = cookie {
                u16(m, ext::COOKIE);
                vec(m, 2, |m| vec(m, 2, |m| m.extend_from_slice(c)));
            }
            u16(m, ext::KEY_SHARE);
            vec(m, 2, |m| {
                vec(m, 2, |m| {
                    for (g, k) in shares {
                        u16(m, *g);
                        vec(m, 2, |m| m.extend_from_slice(k));
                    }
                })
            });
        });
    })
}

use crate::schedule::Suite;

/// A ServerHello, or a HelloRetryRequest (`retry`).
#[derive(Debug)]
pub struct ServerHello<'a> {
    pub retry: bool,
    pub session_id: &'a [u8],
    pub suite: u16,
    pub version: Option<u16>,
    /// key_share: the group, and in a ServerHello the server's share (empty in a HelloRetryRequest).
    pub share: Option<(u16, &'a [u8])>,
    pub cookie: Option<&'a [u8]>,
}

/// Reads a ServerHello's body.
pub fn server_hello(body: &[u8]) -> Result<ServerHello<'_>, Alert> {
    let mut r = Rd::new(body);
    if r.u16()? != 0x0303 {
        return Err(Alert::PROTOCOL_VERSION);
    }
    let retry = r.take(32)? == RETRY_RANDOM;
    let session_id = r.vec(1)?;
    let suite = r.u16()?;
    if r.u8()? != 0 {
        return Err(Alert::ILLEGAL_PARAMETER);
    }
    let mut sh = ServerHello { retry, session_id, suite, version: None, share: None, cookie: None };
    let mut exts = Rd::new(r.vec(2)?);
    r.end()?;
    let mut seen = 0u8;
    while !exts.is_empty() {
        let ty = exts.u16()?;
        let mut d = Rd::new(exts.vec(2)?);
        let bit = match ty {
            ext::SUPPORTED_VERSIONS => {
                sh.version = Some(d.u16()?);
                1
            }
            ext::KEY_SHARE => {
                let g = d.u16()?;
                sh.share = Some((g, if retry { &[] } else { d.vec(2)? }));
                2
            }
            ext::COOKIE if retry => {
                let c = d.vec(2)?;
                if c.is_empty() {
                    return Err(Alert::DECODE_ERROR);
                }
                sh.cookie = Some(c);
                4
            }
            _ => return Err(Alert::UNSUPPORTED_EXTENSION),
        };
        d.end()?;
        if seen & bit != 0 {
            return Err(Alert::ILLEGAL_PARAMETER);
        }
        seen |= bit;
    }
    Ok(sh)
}

/// The extension types a ClientHello offers.
pub fn offered(hello: &[u8]) -> Result<Vec<u16>, Alert> {
    let mut r = Rd::new(hello.get(4..).ok_or(Alert::DECODE_ERROR)?);
    r.take(2 + 32)?;
    r.vec(1)?;
    r.vec(2)?;
    r.vec(1)?;
    let mut exts = Rd::new(r.vec(2)?);
    r.end()?;
    let mut types = Vec::new();
    while !exts.is_empty() {
        types.push(exts.u16()?);
        exts.vec(2)?;
    }
    Ok(types)
}

/// Checks EncryptedExtensions (§ 4.2): only extensions the ClientHello
/// offered may come back, and never those that belong to the ServerHello or
/// to the client alone.
pub fn encrypted_extensions(body: &[u8], offered: &[u16]) -> Result<(), Alert> {
    let mut r = Rd::new(body);
    let mut exts = Rd::new(r.vec(2)?);
    r.end()?;
    while !exts.is_empty() {
        let ty = exts.u16()?;
        exts.vec(2)?;
        match ty {
            ext::KEY_SHARE | ext::SUPPORTED_VERSIONS | ext::COOKIE | ext::SIGNATURE_ALGORITHMS => {
                return Err(Alert::ILLEGAL_PARAMETER);
            }
            _ if !offered.contains(&ty) => return Err(Alert::UNSUPPORTED_EXTENSION),
            _ => {}
        }
    }
    Ok(())
}

/// A server's Certificate: the certificates' DER, the leaf first.
pub fn certificate(body: &[u8]) -> Result<Vec<&[u8]>, Alert> {
    let mut r = Rd::new(body);
    if !r.vec(1)?.is_empty() {
        return Err(Alert::ILLEGAL_PARAMETER); // a server's request context is empty
    }
    let mut list = Rd::new(r.vec(3)?);
    r.end()?;
    let mut certs = Vec::new();
    while !list.is_empty() {
        let der = list.vec(3)?;
        list.vec(2)?; // per-certificate extensions (OCSP, SCT): none asked for, skipped
        if der.is_empty() {
            return Err(Alert::DECODE_ERROR);
        }
        certs.push(der);
    }
    if certs.is_empty() {
        return Err(Alert::DECODE_ERROR); // § 4.4.2.4: a server always sends one
    }
    Ok(certs)
}

/// A CertificateRequest: its context (echoed in the client's empty Certificate).
pub fn certificate_request(body: &[u8]) -> Result<&[u8], Alert> {
    let mut r = Rd::new(body);
    let context = r.vec(1)?;
    r.vec(2)?;
    r.end()?;
    Ok(context)
}

/// The client's Certificate when asked but anonymous: the context, no certificates.
pub fn empty_certificate(context: &[u8]) -> Vec<u8> {
    message(hs::CERTIFICATE, |m| {
        vec(m, 1, |m| m.extend_from_slice(context));
        vec(m, 3, |_| {});
    })
}

/// A CertificateVerify: the scheme and the signature.
pub fn certificate_verify(body: &[u8]) -> Result<(u16, &[u8]), Alert> {
    let mut r = Rd::new(body);
    let s = r.u16()?;
    let sig = r.vec(2)?;
    r.end()?;
    Ok((s, sig))
}

/// What a server's CertificateVerify signs (§ 4.4.3): 64 spaces, the context string, a zero, the transcript's hash.
pub fn signed_content(transcript_hash: &[u8]) -> Vec<u8> {
    let mut c = alloc::vec![0x20u8; 64];
    c.extend_from_slice(b"TLS 1.3, server CertificateVerify\0");
    c.extend_from_slice(transcript_hash);
    c
}
