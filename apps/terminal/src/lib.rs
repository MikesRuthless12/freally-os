// SPDX-License-Identifier: Apache-2.0
//! Freally Terminal (TASK-060, FR-032): a VT/xterm subset in Noto Sans Mono.
//! This library is its logic, host-tested; `main.rs` is the app.
//!
//! - `vt`: bytes to actions (DEC's parser);
//! - `grid`: the screen those actions build, with 10,000 lines of scrollback;
//! - `render`: the grid drawn in Noto Sans Mono, the theme's colours, the cursor.
#![no_std]

extern crate alloc;

pub mod grid;
pub mod render;
pub mod vt;
