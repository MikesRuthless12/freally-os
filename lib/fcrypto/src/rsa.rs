// SPDX-License-Identifier: Apache-2.0
//! RSA signature verification (RFC 8017): RSASSA-PKCS1-v1_5 (§ 8.2.2; what
//! certificates are mostly signed with) and RSASSA-PSS (§ 9.1.2; what TLS
//! 1.3's handshake signs with), over SHA-256, SHA-384 and SHA-512, PSS with
//! MGF1 of the same hash and a salt as long as the hash (as TLS 1.3 asks).
//! Keys of 2,048 to 4,096 bits. Verification only: public values, see [`crate::bignum`].

use crate::bignum::{Mont, Uint};
use crate::sha2::{Sha256, Sha512};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hash {
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    pub fn size(self) -> usize {
        match self {
            Hash::Sha256 => 32,
            Hash::Sha384 => 48,
            Hash::Sha512 => 64,
        }
    }

    /// The hash of the concatenation of `parts`, in the first [`Hash::size`] bytes.
    fn of(self, parts: &[&[u8]]) -> [u8; 64] {
        let mut out = [0u8; 64];
        match self {
            Hash::Sha256 => {
                let mut h = Sha256::new();
                parts.iter().for_each(|p| h.update(p));
                out[..32].copy_from_slice(&h.finish());
            }
            Hash::Sha384 | Hash::Sha512 => {
                let mut h = if self == Hash::Sha384 { Sha512::new_384() } else { Sha512::new() };
                parts.iter().for_each(|p| h.update(p));
                let d = h.finish();
                out[..self.size()].copy_from_slice(&d[..self.size()]);
            }
        }
        out
    }

    /// The DER `DigestInfo` that comes before the digest in PKCS#1 v1.5 (RFC 8017 § 9.2, note 1).
    fn digest_info(self) -> &'static [u8] {
        match self {
            Hash::Sha256 => &[
                0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05, 0x00,
                0x04, 0x20,
            ],
            Hash::Sha384 => &[
                0x30, 0x41, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x02, 0x05, 0x00,
                0x04, 0x30,
            ],
            Hash::Sha512 => &[
                0x30, 0x51, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x03, 0x05, 0x00,
                0x04, 0x40,
            ],
        }
    }
}

/// A public key: its modulus and exponent.
#[derive(Clone, Copy, Debug)]
pub struct PublicKey {
    mont: Mont,
    e: Uint,
    /// The modulus's length in bytes, and in bits.
    k: usize,
    bits: usize,
}

impl PublicKey {
    /// From the modulus and exponent (big-endian): 2,048 to 4,096 bits, an
    /// odd exponent of at least 3; `None` otherwise.
    pub fn new(n: &[u8], e: &[u8]) -> Option<PublicKey> {
        let nu = Uint::from_be(n)?;
        let eu = Uint::from_be(e)?;
        let bits = nu.bits();
        if !(2048..=4096).contains(&bits) || eu.bits() < 2 || eu.l[0] & 1 == 0 || !eu.lt(&nu) {
            return None;
        }
        Some(PublicKey { mont: Mont::new(nu)?, e: eu, k: bits.div_ceil(8), bits })
    }

    /// s^e mod n as k bytes (RSAVP1, § 5.2.2), into `em`; `false` if the
    /// signature isn't k bytes or isn't below n.
    fn open(&self, sig: &[u8], em: &mut [u8; 512]) -> bool {
        if sig.len() != self.k {
            return false;
        }
        let Some(s) = Uint::from_be(sig) else { return false };
        if !s.lt(&self.mont.m) {
            return false;
        }
        self.mont.pow(&s, &self.e).to_be(&mut em[..self.k]);
        true
    }

    /// RSASSA-PKCS1-v1_5 (§ 8.2.2): `digest` signed with `hash`.
    pub fn verify_pkcs1(&self, hash: Hash, digest: &[u8], sig: &[u8]) -> bool {
        let mut em = [0u8; 512];
        if digest.len() != hash.size() || !self.open(sig, &mut em) {
            return false;
        }
        let (k, info) = (self.k, hash.digest_info());
        let t = info.len() + digest.len();
        if k < t + 11 {
            return false;
        }
        // 00 01 FF…FF 00 DigestInfo digest, compared whole (§ 9.2: encode and compare).
        let ps = k - t - 3;
        em[0] == 0
            && em[1] == 1
            && em[2..2 + ps].iter().all(|&b| b == 0xFF)
            && em[2 + ps] == 0
            && &em[3 + ps..3 + ps + info.len()] == info
            && &em[k - digest.len()..k] == digest
    }

    /// RSASSA-PSS (§ 9.1.2, EMSA-PSS-VERIFY § 9.1.2): `digest` signed with
    /// `hash`, MGF1 of the same hash, a salt as long as the hash.
    pub fn verify_pss(&self, hash: Hash, digest: &[u8], sig: &[u8]) -> bool {
        let mut buf = [0u8; 512];
        if digest.len() != hash.size() || !self.open(sig, &mut buf) {
            return false;
        }
        let (h_len, s_len) = (hash.size(), hash.size());
        let em_bits = self.bits - 1;
        let em_len = em_bits.div_ceil(8);
        // A modulus of 8k+1 bits gives a leading zero byte that isn't the message's.
        let lead = self.k - em_len;
        if buf[..lead].iter().any(|&b| b != 0) {
            return false;
        }
        let em = &buf[lead..self.k];
        if em_len < h_len + s_len + 2 || em[em_len - 1] != 0xBC {
            return false;
        }
        let db_len = em_len - h_len - 1;
        let (masked, h) = (&em[..db_len], &em[db_len..db_len + h_len]);
        let top = 8 * em_len - em_bits;
        if top > 0 && masked[0] >> (8 - top) != 0 {
            return false;
        }
        // DB = maskedDB XOR MGF1(H), its leftmost bits cleared.
        let mut db = [0u8; 512];
        let mut counter = 0u32;
        let mut at = 0;
        while at < db_len {
            let block = hash.of(&[h, &counter.to_be_bytes()]);
            for (i, b) in block[..h_len].iter().enumerate().take(db_len - at) {
                db[at + i] = masked[at + i] ^ b;
            }
            at += h_len;
            counter += 1;
        }
        db[0] &= 0xFF >> top;
        let ps = db_len - s_len - 1;
        if db[..ps].iter().any(|&b| b != 0) || db[ps] != 1 {
            return false;
        }
        let salt = &db[ps + 1..db_len];
        let h2 = hash.of(&[&[0u8; 8], digest, salt]);
        &h2[..h_len] == h
    }
}
