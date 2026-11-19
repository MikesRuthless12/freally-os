// SPDX-License-Identifier: Apache-2.0
//! drv-i8042's logic, host-tested; `main.rs` is the driver.
#![no_std]

extern crate alloc;

pub mod quirks;
