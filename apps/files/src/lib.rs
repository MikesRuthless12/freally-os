// SPDX-License-Identifier: Apache-2.0
//! Files, v0 (TASK-105, FR-049): browse the home, open folders, rename,
//! move, delete to the account's bin (and restore or empty it), make a
//! folder, and drag between two Files windows. `disk`, `bin` and `browser`
//! are its models, tested on the host; the program (`main.rs`) draws them
//! with `fapp`.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod bin;
pub mod browser;
pub mod disk;

/// Key usages Files reads.
pub mod key {
    pub use app_session::key::{BACKSPACE, DOWN, ENTER, ESCAPE, SHIFT, TAB, UP};
    pub const N: u16 = 0x11;
    pub const SPACE: u16 = 0x2C;
    pub const F2: u16 = 0x3B;
    pub const HOME: u16 = 0x4A;
    pub const DELETE: u16 = 0x4C;
    pub const END: u16 = 0x4D;
    /// Ctrl, in stage's modifier bits.
    pub const CTRL: u8 = 1 << 1;
}
