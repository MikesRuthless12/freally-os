// SPDX-License-Identifier: Apache-2.0
//! desktop (TASK-059, FR-030): the wallpaper, the Rack and the launcher, as
//! stage's client. This library is its logic, host-tested; `main.rs` is the
//! service.
//!
//! - `rack`: the Rack's keys, LEDs and clock well, laid out;
//! - `clock`: the 12-hour clock;
//! - `launcher`: typing to filter the apps, choosing, starting.
#![no_std]

extern crate alloc;

pub mod clock;
pub mod launcher;
pub mod rack;
