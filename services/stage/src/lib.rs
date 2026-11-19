// SPDX-License-Identifier: Apache-2.0
//! stage (TASK-057, FR-028): the compositor and window manager. This library
//! is its logic, each part host-tested; `main.rs` is the service that runs
//! it against the framebuffer, the input driver and its clients.
//!
//! - `windows`: where each window is, how they stack, which has focus;
//! - `shortcuts`: Super, Super+M, Super+T, Alt+Tab, Alt+F4;
//! - `input`: keys and the mouse routed to windows, drags, focus;
//! - `compose`: the back buffer, built where damaged, copied to the screen.
#![no_std]

extern crate alloc;

pub mod compose;
pub mod cursor;
pub mod idle;
pub mod input;
pub mod shortcuts;
pub mod windows;
