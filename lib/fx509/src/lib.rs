// SPDX-License-Identifier: Apache-2.0
//! fx509 (TASK-179, FR-060): what TLS 1.3 needs of certificates. [`der`]
//! reads DER, every length checked before a byte is read.
#![cfg_attr(not(test), no_std)]

pub mod cert;
pub mod der;
pub mod verify;

#[cfg(test)]
mod tests;
