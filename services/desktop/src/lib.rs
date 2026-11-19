// SPDX-License-Identifier: Apache-2.0
//! desktop (TASK-059, FR-030): the wallpaper, the Rack and the launcher, as
//! stage's client. This library is its logic, host-tested; `main.rs` is the
//! service.
//!
//! - `rack`: the Rack's keys, LEDs and clock well, laid out;
//! - `clock`: the 12-hour clock;
//! - `launcher`: typing to filter the apps, choosing, starting;
//! - `handoff`: what a swap hands the next build (dev images);
//! - `saver`: the four screensavers and Reduce Motion's still F.
#![no_std]

extern crate alloc;

pub mod clock;
pub mod handoff;
pub mod launcher;
pub mod rack;
pub mod saver;
