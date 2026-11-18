// SPDX-License-Identifier: Apache-2.0
//! Freally Shell (TASK-061, FR-033): line editing, history, and the
//! built-ins. It runs inside Freally Terminal (there are no other programs
//! to start yet): the terminal hands it the bytes keys make and draws the
//! bytes it answers with.
//!
//! - `line`: the line editor and its history;
//! - `builtins`: the commands, parsed and run against a `System`.
#![no_std]

extern crate alloc;

pub mod builtins;
pub mod line;
