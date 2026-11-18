// SPDX-License-Identifier: Apache-2.0
//! fstd: the Freally standard library (TASK-029's skeleton): logging, time,
//! owned handles, channels and the service-call client, and `program!`.
//!
//! A program:
//! ```ignore
//! #![cfg_attr(target_os = "none", no_std, no_main)]
//! extern crate alloc;
//! fstd::program!(run);
//! fn run(bootstrap: u32) -> i64 { fstd::info!("hello"); 0 }
//! ```
#![no_std]

extern crate alloc;

pub mod handle;
pub mod ipc;
pub mod log;
pub mod process;
pub mod serial_json;
pub mod service;
pub mod time;

pub use fcore_abi::{FErr, well_known};
pub use frt::sys;

/// Declares a program's entry. On Freally OS, `_start` calls `$run` with the
/// bootstrap argument and exits with its result. On the host the program is
/// only compiled (its logic is tested as library code).
#[macro_export]
macro_rules! program {
    ($run:path) => {
        #[cfg(target_os = "none")]
        #[unsafe(no_mangle)]
        pub extern "C" fn _frt_main(arg: u64) -> ! {
            let code: i64 = $run(arg as u32);
            $crate::process::exit(code)
        }

        #[cfg(not(target_os = "none"))]
        fn main() {
            let _ = $run as fn(u32) -> i64;
        }
    };
}
