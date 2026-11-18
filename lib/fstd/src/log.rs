// SPDX-License-Identifier: Apache-2.0
//! Logging. Once a service has a logger connection (`attach`, or `connect`
//! after `service::start`), each line is a one-way `write` to the logger; until
//! then it goes to the kernel log through `debug_log`, which only the dev
//! flavour has. The macros format like `format!`.

use crate::ipc::Channel;
use alloc::string::String;
use core::cell::RefCell;
use fwire::proto::logger::Write;

pub use fwire::proto::logger::{DEBUG, ERROR, INFO, TRACE, WARN};

struct Global(RefCell<Option<Channel>>);
// SAFETY: single-threaded programs (see `service`).
unsafe impl Sync for Global {}
static LOGGER: Global = Global(RefCell::new(None));

/// Sends this program's lines to the logger over `ch` from now on.
pub fn attach(ch: Channel) {
    *LOGGER.0.borrow_mut() = Some(ch);
}

/// Connects to the logger through conductor and attaches (services).
pub fn connect() -> Result<(), crate::ipc::CallError> {
    attach(crate::service::connect("logger")?);
    Ok(())
}

/// Writes one line at `level`.
pub fn write(level: u8, msg: &str) {
    let logger = LOGGER.0.borrow();
    if let Some(ch) = logger.as_ref()
        && let Ok(m) = fwire::event(fwire::proto::logger::WRITE, &Write { level, msg: String::from(msg) })
        && ch.send(&m, alloc::vec::Vec::new()).is_ok()
    {
        return;
    }
    drop(logger);
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
