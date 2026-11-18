// SPDX-License-Identifier: Apache-2.0
//! Logging. Until the logger service exists (Phase 2), a line goes to the
//! kernel log through `debug_log`, which only the dev flavour has; elsewhere
//! it's dropped. The macros format like `format!`.

use alloc::string::String;

pub use fwire::proto::logger::{DEBUG, ERROR, INFO, TRACE, WARN};

/// Writes one line at `level`.
pub fn write(level: u8, msg: &str) {
    let tag = match level {
        ERROR => "error: ",
        WARN => "warning: ",
        _ => "",
    };
    let mut line = String::with_capacity(tag.len() + msg.len());
    line.push_str(tag);
    line.push_str(msg);
    crate::sys::debug_log(line.as_bytes());
}

#[macro_export]
macro_rules! info {
    ($($a:tt)*) => { $crate::log::write($crate::log::INFO, &alloc::format!($($a)*)) };
}

#[macro_export]
macro_rules! warn {
    ($($a:tt)*) => { $crate::log::write($crate::log::WARN, &alloc::format!($($a)*)) };
}

#[macro_export]
macro_rules! error {
    ($($a:tt)*) => { $crate::log::write($crate::log::ERROR, &alloc::format!($($a)*)) };
}
