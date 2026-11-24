// SPDX-License-Identifier: Apache-2.0
//! Settings (TASK-104, FR-048): two panes, Personalisation, Accounts and
//! About, with the fleet's Restore Defaults, OK, Cancel and Apply. `shell`,
//! `personalisation`, `accounts` and `about` are its models, tested on the
//! host; the program (`main.rs`) draws them with `fapp`.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod about;
pub mod accounts;
pub mod hardware;
pub mod personalisation;
pub mod shell;
pub mod time;
pub mod updates;

/// Key usages Settings reads.
pub mod key {
    pub use app_session::key::{BACKSPACE, DOWN, ENTER, ESCAPE, SHIFT, TAB, UP};
    pub const SPACE: u16 = 0x2C;
    pub const RIGHT: u16 = 0x4F;
    pub const LEFT: u16 = 0x50;
    pub const HOME: u16 = 0x4A;
    pub const END: u16 = 0x4D;
}
