// SPDX-License-Identifier: Apache-2.0
//! ftls (TASK-180, FR-060): a TLS 1.3 client (RFC 8446), written from the RFC.
//!
//! - [`schedule`]: the key schedule (HKDF-Expand-Label, the transcript hash, Finished).
//! - [`record`]: record protection with AES-128-GCM, AES-256-GCM or ChaCha20-Poly1305.
//! - [`msg`]: the handshake messages a client writes and reads.
//! - [`client`]: the handshake and the connection, sans I/O.
//!
//! The server's chain is checked by `fx509` against the roots the caller gives.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod client;
pub mod msg;
pub mod record;
pub mod roots;
pub mod schedule;

pub use client::{Client, Error};

/// An alert's description (§ 6).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Alert(pub u8);

impl Alert {
    pub const CLOSE_NOTIFY: Alert = Alert(0);
    pub const UNEXPECTED_MESSAGE: Alert = Alert(10);
    pub const BAD_RECORD_MAC: Alert = Alert(20);
    pub const RECORD_OVERFLOW: Alert = Alert(22);
    pub const HANDSHAKE_FAILURE: Alert = Alert(40);
    pub const BAD_CERTIFICATE: Alert = Alert(42);
    pub const UNSUPPORTED_CERTIFICATE: Alert = Alert(43);
    pub const CERTIFICATE_EXPIRED: Alert = Alert(45);
    pub const CERTIFICATE_UNKNOWN: Alert = Alert(46);
    pub const ILLEGAL_PARAMETER: Alert = Alert(47);
    pub const UNKNOWN_CA: Alert = Alert(48);
    pub const DECODE_ERROR: Alert = Alert(50);
    pub const DECRYPT_ERROR: Alert = Alert(51);
    pub const PROTOCOL_VERSION: Alert = Alert(70);
    pub const INSUFFICIENT_SECURITY: Alert = Alert(71);
    pub const INTERNAL_ERROR: Alert = Alert(80);
    pub const USER_CANCELED: Alert = Alert(90);
    pub const MISSING_EXTENSION: Alert = Alert(109);
    pub const UNSUPPORTED_EXTENSION: Alert = Alert(110);

    pub fn name(self) -> &'static str {
        match self.0 {
            0 => "close_notify",
            10 => "unexpected_message",
            20 => "bad_record_mac",
            22 => "record_overflow",
            40 => "handshake_failure",
            42 => "bad_certificate",
            43 => "unsupported_certificate",
            45 => "certificate_expired",
            46 => "certificate_unknown",
            47 => "illegal_parameter",
            48 => "unknown_ca",
            50 => "decode_error",
            51 => "decrypt_error",
            70 => "protocol_version",
            71 => "insufficient_security",
            80 => "internal_error",
            90 => "user_canceled",
            109 => "missing_extension",
            110 => "unsupported_extension",
            _ => "an alert",
        }
    }
}

impl core::fmt::Debug for Alert {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} ({})", self.name(), self.0)
    }
}

#[cfg(test)]
mod tests;
