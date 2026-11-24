// SPDX-License-Identifier: Apache-2.0
//! Freally's own cryptography, written from the specifications.
//!
//! - [`sha2`]: SHA-256, SHA-384 and SHA-512 (FIPS 180-4).
//! - [`aes`], [`gcm`]: AES-128/256 (FIPS 197) and AES-GCM (SP 800-38D), for TLS 1.3.
//! - [`rsa`] on [`bignum`]: RSA signature verification (RFC 8017: PKCS#1 v1.5, PSS).
//! - [`sha1`]: SHA-1 (FIPS 180-4), **for the WebSocket handshake only** (RFC 6455).
//!   It's broken for anything that needs collision resistance; never use it for that.
//! - [`ed25519`]: Ed25519 signing and verification (RFC 8032), constant-time
//!   field arithmetic in radix 2^51 with no secret-indexed tables.
//! - [`crc32c`]: CRC-32C (Castagnoli, RFC 3720), for integrity, never authentication.
//! - [`ct`]: constant-time comparison and selection.
//!
//! Every algorithm is tested against its official vectors (`tests/vectors.rs`).
#![no_std]

pub mod aead;
pub mod aes;
pub mod argon2;
pub mod bignum;
pub mod blake2b;
pub mod chacha;
pub mod crc32c;
pub mod ct;
pub mod ed25519;
pub mod gcm;
pub mod hkdf;
pub mod poly1305;
pub mod rsa;
pub mod sha1;
pub mod sha2;
#[cfg(all(target_arch = "x86_64", feature = "shani"))]
pub mod sha256_ni;
pub mod x25519;
