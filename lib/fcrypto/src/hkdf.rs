// SPDX-License-Identifier: Apache-2.0
//! HMAC-SHA-256 (RFC 2104, FIPS 198-1) and HKDF-SHA-256 (RFC 5869): the
//! account key's derivation (`kek = HKDF(Argon2id(password, salt),
//! "freally/account/kek/v1")`, PRD § 10) and `frandom`'s mixing.

use crate::sha2::Sha256;

/// A running HMAC-SHA-256.
#[derive(Clone)]
pub struct Hmac {
    inner: Sha256,
    outer: Sha256,
}

impl Hmac {
    pub fn new(key: &[u8]) -> Hmac {
        let mut k = [0u8; 64];
        if key.len() > 64 {
            k[..32].copy_from_slice(&crate::sha2::sha256(key));
        } else {
            k[..key.len()].copy_from_slice(key);
        }
        let (mut inner, mut outer) = (Sha256::new(), Sha256::new());
        inner.update(&k.map(|b| b ^ 0x36));
        outer.update(&k.map(|b| b ^ 0x5c));
        Hmac { inner, outer }
    }

    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    pub fn finish(self) -> [u8; 32] {
        let mut outer = self.outer;
        outer.update(&self.inner.finish());
        outer.finish()
    }
}

/// HMAC-SHA-256 of `data` under `key`.
pub fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut h = Hmac::new(key);
    h.update(data);
    h.finish()
}

/// HKDF-Extract: the pseudorandom key from the input keying material (an
/// empty salt is 32 zero bytes, § 2.2).
pub fn extract(salt: &[u8], ikm: &[u8]) -> [u8; 32] {
    hmac(if salt.is_empty() { &[0u8; 32] } else { salt }, ikm)
}

/// The output is longer than HKDF can give (255 × 32 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooLong;

/// HKDF-Expand: `out.len()` bytes from `prk` and `info`.
pub fn expand(prk: &[u8; 32], info: &[u8], out: &mut [u8]) -> Result<(), TooLong> {
    if out.len() > 255 * 32 {
        return Err(TooLong);
    }
    let mut t = [0u8; 32];
    for (i, chunk) in out.chunks_mut(32).enumerate() {
        let mut h = Hmac::new(prk);
        if i > 0 {
            h.update(&t);
        }
        h.update(info);
        h.update(&[(i + 1) as u8]);
        t = h.finish();
        chunk.copy_from_slice(&t[..chunk.len()]);
    }
    Ok(())
}

/// Extract then expand, into `out`.
pub fn hkdf(salt: &[u8], ikm: &[u8], info: &[u8], out: &mut [u8]) -> Result<(), TooLong> {
    expand(&extract(salt, ikm), info, out)
}
