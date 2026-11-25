// SPDX-License-Identifier: Apache-2.0
//! cdpd, the CDP test door (TASK-063, FR-039, e2e images only). This library
//! is its pure part, host-tested: the desktop's DOM built from stage's
//! windows and the trees they published, CDP's selectors over it, and the
//! answers' JSON; and WebSocket (`ws`) for its port on the network
//! (TASK-184). `main.rs` fetches from stage, conductor and the logger.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod dom;
pub mod message;
pub mod ws;
