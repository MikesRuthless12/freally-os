// SPDX-License-Identifier: Apache-2.0
//! Authenticated encryption: ChaCha20-Poly1305 (RFC 8439 § 2.8) and
//! XChaCha20-Poly1305 (draft-irtf-cfrg-xchacha § 2.3), which FreallyFS and
//! `.facct` seal with (a 24-byte nonce is safe to choose at random).
//!
//! `open` checks the tag (in constant time) before it decrypts a byte: a
//! forged or damaged message is refused whole and the buffer left as it was.

use crate::{chacha, ct, poly1305::Poly1305};

/// The tag is wrong: the message was changed, or the key or nonce is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Forged;

fn tag(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], ciphertext: &[u8]) -> [u8; 16] {
    let mut otk = [0u8; 32];
    otk.copy_from_slice(&chacha::block(key, 0, nonce)[..32]);
    let mut p = Poly1305::new(&otk);
    let zeros = [0u8; 16];
    p.update(aad);
    p.update(&zeros[..(16 - aad.len() % 16) % 16]);
    p.update(ciphertext);
    p.update(&zeros[..(16 - ciphertext.len() % 16) % 16]);
    p.update(&(aad.len() as u64).to_le_bytes());
    p.update(&(ciphertext.len() as u64).to_le_bytes());
    p.finish()
}

/// Encrypts `data` in place; returns the tag over `aad` and the ciphertext.
pub fn seal(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], data: &mut [u8]) -> [u8; 16] {
    chacha::xor(key, 1, nonce, data);
    tag(key, nonce, aad, data)
}

/// Checks `tag`, then decrypts `data` in place; a wrong tag leaves `data` as it was.
pub fn open(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], data: &mut [u8], t: &[u8; 16]) -> Result<(), Forged> {
    if !ct::eq(&tag(key, nonce, aad, data), t) {
        return Err(Forged);
    }
    chacha::xor(key, 1, nonce, data);
    Ok(())
}

/// XChaCha's subkey and 12-byte nonce from the 24-byte nonce.
fn extend(key: &[u8; 32], nonce: &[u8; 24]) -> ([u8; 32], [u8; 12]) {
    let mut first = [0u8; 16];
    first.copy_from_slice(&nonce[..16]);
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&nonce[16..]);
    (chacha::hchacha20(key, &first), n)
}

/// XChaCha20-Poly1305's seal.
pub fn xseal(key: &[u8; 32], nonce: &[u8; 24], aad: &[u8], data: &mut [u8]) -> [u8; 16] {
    let (k, n) = extend(key, nonce);
    seal(&k, &n, aad, data)
}

/// XChaCha20-Poly1305's open.
pub fn xopen(key: &[u8; 32], nonce: &[u8; 24], aad: &[u8], data: &mut [u8], t: &[u8; 16]) -> Result<(), Forged> {
    let (k, n) = extend(key, nonce);
    open(&k, &n, aad, data, t)
}
