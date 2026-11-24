// SPDX-License-Identifier: Apache-2.0
//! TLS 1.3's record protection (RFC 8446 § 5): the traffic keys from a
//! secret, the per-record nonce, TLSInnerPlaintext with its content type, and
//! the record header as the AEAD's additional data.

use alloc::vec::Vec;

use crate::Alert;
use crate::schedule::{Secret, Suite, expand_label, wipe};

pub const HEADER: usize = 5;
/// The most plaintext a record carries (2^14).
pub const MAX_PLAIN: usize = 1 << 14;
/// The longest protected record body (2^14 + 256).
pub const MAX_BODY: usize = MAX_PLAIN + 256;
pub const TAG: usize = 16;

pub mod content {
    pub const CHANGE_CIPHER_SPEC: u8 = 20;
    pub const ALERT: u8 = 21;
    pub const HANDSHAKE: u8 = 22;
    pub const APPLICATION_DATA: u8 = 23;
}

enum Cipher {
    Gcm(alloc::boxed::Box<fcrypto::gcm::AesGcm>),
    ChaCha([u8; 32]),
}

/// One direction's keys and its record count.
pub struct Protection {
    cipher: Cipher,
    iv: [u8; 12],
    seq: u64,
}

impl Drop for Protection {
    fn drop(&mut self) {
        if let Cipher::ChaCha(k) = &mut self.cipher {
            wipe(k);
        }
        wipe(&mut self.iv);
    }
}

impl Protection {
    /// The key and IV of a traffic secret (§ 7.3).
    pub fn new(suite: Suite, secret: &Secret) -> Protection {
        let mut key = [0u8; 32];
        let n = suite.key_len();
        expand_label(suite, secret, b"key", b"", &mut key[..n]);
        let mut iv = [0u8; 12];
        expand_label(suite, secret, b"iv", b"", &mut iv);
        let cipher = match suite {
            Suite::ChaCha20Poly1305Sha256 => Cipher::ChaCha(key),
            _ => Cipher::Gcm(alloc::boxed::Box::new(fcrypto::gcm::AesGcm::new(&key[..n]).expect("16 or 32 bytes"))),
        };
        wipe(&mut key);
        Protection { cipher, iv, seq: 0 }
    }

    /// The IV with the record's sequence number XORed into its last 8 bytes (§ 5.3).
    /// The count is never near 2^64: that many records is more data than exists.
    fn nonce(&self) -> [u8; 12] {
        let mut n = self.iv;
        for (b, s) in n[4..].iter_mut().zip(self.seq.to_be_bytes()) {
            *b ^= s;
        }
        n
    }

    /// Seals `payload` (at most 2^14 bytes) of content type `ty` as one record onto `out`.
    pub fn seal(&mut self, ty: u8, payload: &[u8], out: &mut Vec<u8>) {
        assert!(payload.len() <= MAX_PLAIN, "a record's plaintext is at most 2^14 bytes");
        let len = payload.len() + 1 + TAG;
        let header = [content::APPLICATION_DATA, 3, 3, (len >> 8) as u8, len as u8];
        out.extend_from_slice(&header);
        let start = out.len();
        out.extend_from_slice(payload);
        out.push(ty);
        let nonce = self.nonce();
        let tag = match &self.cipher {
            Cipher::Gcm(g) => g.seal(&nonce, &header, &mut out[start..]),
            Cipher::ChaCha(k) => fcrypto::aead::seal(k, &nonce, &header, &mut out[start..]),
        };
        out.extend_from_slice(&tag);
        self.seq += 1;
    }

    /// Opens one protected record in place: its inner content type and its
    /// plaintext's length, the padding removed.
    pub fn open(&mut self, header: &[u8; HEADER], body: &mut [u8]) -> Result<(u8, usize), Alert> {
        if body.len() > MAX_BODY {
            return Err(Alert::RECORD_OVERFLOW);
        }
        if body.len() < TAG {
            return Err(Alert::BAD_RECORD_MAC);
        }
        let (data, tag) = body.split_at_mut(body.len() - TAG);
        let tag: &[u8; TAG] = (&*tag).try_into().expect("16 bytes");
        let nonce = self.nonce();
        match &self.cipher {
            Cipher::Gcm(g) => g.open(&nonce, header, data, tag).map_err(|_| Alert::BAD_RECORD_MAC)?,
            Cipher::ChaCha(k) => {
                fcrypto::aead::open(k, &nonce, header, data, tag).map_err(|_| Alert::BAD_RECORD_MAC)?
            }
        }
        self.seq += 1;
        // The content type is the last byte that isn't padding; all padding is a peer's error.
        let end = data.iter().rposition(|&b| b != 0).ok_or(Alert::UNEXPECTED_MESSAGE)?;
        if end > MAX_PLAIN {
            return Err(Alert::RECORD_OVERFLOW);
        }
        Ok((data[end], end))
    }
}

/// A record in the clear (the ClientHello, an alert before keys): `ty`,
/// the legacy version, the length, the payload.
pub fn plain(ty: u8, version: u16, payload: &[u8], out: &mut Vec<u8>) {
    out.push(ty);
    out.extend_from_slice(&version.to_be_bytes());
    out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    out.extend_from_slice(payload);
}
