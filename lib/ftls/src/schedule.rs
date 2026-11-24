// SPDX-License-Identifier: Apache-2.0
//! TLS 1.3's key schedule (RFC 8446 § 7.1): HKDF-Expand-Label, Derive-Secret,
//! the transcript hash and the Finished MAC, for the three cipher suites.
//! No pre-shared keys: the early secret is always HKDF-Extract(0, 0).

use fcrypto::hkdf;
use fcrypto::sha2::{Sha256, Sha512};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Suite {
    Aes128GcmSha256,
    Aes256GcmSha384,
    ChaCha20Poly1305Sha256,
}

impl Suite {
    /// The suites offered, in the client's order of preference: ChaCha20 first,
    /// the fastest of the three while AES runs in software.
    pub const OFFERED: [Suite; 3] = [Suite::ChaCha20Poly1305Sha256, Suite::Aes128GcmSha256, Suite::Aes256GcmSha384];

    pub fn id(self) -> u16 {
        match self {
            Suite::Aes128GcmSha256 => 0x1301,
            Suite::Aes256GcmSha384 => 0x1302,
            Suite::ChaCha20Poly1305Sha256 => 0x1303,
        }
    }

    pub fn from_id(id: u16) -> Option<Suite> {
        Suite::OFFERED.into_iter().find(|s| s.id() == id)
    }

    pub fn hash_len(self) -> usize {
        if self == Suite::Aes256GcmSha384 { 48 } else { 32 }
    }

    pub fn key_len(self) -> usize {
        if self == Suite::Aes128GcmSha256 { 16 } else { 32 }
    }
}

/// A secret of the suite's hash length (32 or 48 bytes), wiped when dropped.
#[derive(Clone)]
pub struct Secret {
    b: [u8; 48],
    len: usize,
}

impl Secret {
    pub fn new(b: &[u8]) -> Secret {
        let mut s = Secret { b: [0; 48], len: b.len() };
        s.b[..b.len()].copy_from_slice(b);
        s
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.b[..self.len]
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        wipe(&mut self.b);
    }
}

/// Zeroes `b` in a way the compiler won't remove.
pub fn wipe(b: &mut [u8]) {
    for x in b.iter_mut() {
        // SAFETY: `x` is a valid, aligned, exclusive reference.
        unsafe { core::ptr::write_volatile(x, 0) };
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

/// HMAC of the parts of `data`, with the suite's hash.
pub fn hmac(suite: Suite, key: &[u8], data: &[&[u8]]) -> Secret {
    if suite.hash_len() == 48 {
        let mut h = hkdf::sha384::Hmac::new(key);
        data.iter().for_each(|d| h.update(d));
        Secret::new(&h.finish())
    } else {
        let mut h = hkdf::Hmac::new(key);
        data.iter().for_each(|d| h.update(d));
        Secret::new(&h.finish())
    }
}

/// HKDF-Extract; an empty salt is the hash's length of zeros (RFC 5869 § 2.2).
pub fn extract(suite: Suite, salt: &[u8], ikm: &[u8]) -> Secret {
    let zeros = [0u8; 48];
    hmac(suite, if salt.is_empty() { &zeros[..suite.hash_len()] } else { salt }, &[ikm])
}

/// HKDF-Expand-Label(secret, label, context, out.len()) (§ 7.1). Labels are this
/// crate's own and contexts at most a hash, so the HkdfLabel always fits.
pub fn expand_label(suite: Suite, secret: &Secret, label: &[u8], context: &[u8], out: &mut [u8]) {
    assert!(label.len() <= 32 && context.len() <= 48, "a label or context longer than TLS uses");
    let mut info = [0u8; 2 + 1 + 6 + 32 + 1 + 48];
    info[..2].copy_from_slice(&(out.len() as u16).to_be_bytes());
    info[2] = (6 + label.len()) as u8;
    info[3..9].copy_from_slice(b"tls13 ");
    let mut at = 9 + label.len();
    info[9..at].copy_from_slice(label);
    info[at] = context.len() as u8;
    at += 1;
    info[at..at + context.len()].copy_from_slice(context);
    let info = &info[..at + context.len()];
    let done = if suite.hash_len() == 48 {
        let prk: &[u8; 48] = secret.as_slice().try_into().expect("a SHA-384 secret");
        hkdf::sha384::expand(prk, info, out)
    } else {
        let prk: &[u8; 32] = secret.as_slice().try_into().expect("a SHA-256 secret");
        hkdf::expand(prk, info, out)
    };
    done.expect("TLS never asks for more than 255 hashes' length");
}

/// Derive-Secret(secret, label, the transcript's hash).
pub fn derive_secret(suite: Suite, secret: &Secret, label: &[u8], transcript_hash: &[u8]) -> Secret {
    let mut out = [0u8; 48];
    let n = suite.hash_len();
    expand_label(suite, secret, label, transcript_hash, &mut out[..n]);
    let s = Secret::new(&out[..n]);
    wipe(&mut out);
    s
}

/// The running hash of the handshake's messages.
#[derive(Clone)]
pub enum Transcript {
    Sha256(Sha256),
    Sha384(Sha512),
}

impl Transcript {
    pub fn new(suite: Suite) -> Transcript {
        if suite.hash_len() == 48 { Transcript::Sha384(Sha512::new_384()) } else { Transcript::Sha256(Sha256::new()) }
    }

    pub fn update(&mut self, m: &[u8]) {
        match self {
            Transcript::Sha256(h) => h.update(m),
            Transcript::Sha384(h) => h.update(m),
        }
    }

    /// The hash of everything so far (the transcript goes on).
    pub fn hash(&self) -> Secret {
        match self.clone() {
            Transcript::Sha256(h) => Secret::new(&h.finish()),
            Transcript::Sha384(h) => Secret::new(&h.finish()[..48]),
        }
    }
}

/// The key schedule's current stage: the handshake secret, then the master secret.
pub struct Schedule {
    suite: Suite,
    pub(crate) secret: Secret,
}

impl Schedule {
    fn next(suite: Suite, from: &Secret, ikm: &[u8]) -> Schedule {
        let empty = Transcript::new(suite).hash();
        let derived = derive_secret(suite, from, b"derived", empty.as_slice());
        Schedule { suite, secret: extract(suite, derived.as_slice(), ikm) }
    }

    /// The handshake secret from the (EC)DHE shared secret (no PSK: the early
    /// secret is HKDF-Extract(0, 0)).
    pub fn handshake(suite: Suite, shared: &[u8]) -> Schedule {
        let zeros = [0u8; 48];
        let early = extract(suite, &[], &zeros[..suite.hash_len()]);
        Schedule::next(suite, &early, shared)
    }

    /// The master secret that follows this handshake secret.
    pub fn master(&self) -> Schedule {
        let zeros = [0u8; 48];
        Schedule::next(self.suite, &self.secret, &zeros[..self.suite.hash_len()])
    }

    /// A traffic secret ("c hs traffic", "s ap traffic" …) at this transcript hash.
    pub fn traffic(&self, label: &[u8], transcript_hash: &Secret) -> Secret {
        derive_secret(self.suite, &self.secret, label, transcript_hash.as_slice())
    }
}

/// A Finished message's verify_data (§ 4.4.4): HMAC(finished_key, the transcript's hash).
pub fn finished(suite: Suite, traffic: &Secret, transcript_hash: &Secret) -> Secret {
    let mut key = [0u8; 48];
    let n = suite.hash_len();
    expand_label(suite, traffic, b"finished", b"", &mut key[..n]);
    let v = hmac(suite, &key[..n], &[transcript_hash.as_slice()]);
    wipe(&mut key);
    v
}

/// The next traffic secret after a KeyUpdate (§ 7.2).
pub fn next_traffic(suite: Suite, secret: &Secret) -> Secret {
    derive_secret(suite, secret, b"traffic upd", b"")
}
