// SPDX-License-Identifier: Apache-2.0
//! The session screens (TASK-102, PRD § 8, FR-045): **Welcome** makes the
//! first account (the Owner) on a disk with none; **Login** opens an
//! account. Each is its own program (`welcome`, `login`), so a password is
//! typed into a process that holds nothing else; the desktop starts the
//! right one while no one is logged in, and each ends once a session has
//! started. `form` and `login` are their models, tested on the host;
//! `screen` draws them.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod form;
pub mod login;
pub mod screen;

/// Key usages the screens read.
pub mod key {
    pub const ENTER: u16 = 0x28;
    pub const ESCAPE: u16 = 0x29;
    pub const BACKSPACE: u16 = 0x2A;
    pub const TAB: u16 = 0x2B;
    pub const DOWN: u16 = 0x51;
    pub const UP: u16 = 0x52;
    /// Shift, in stage's modifier bits.
    pub const SHIFT: u8 = 1;
}
