// SPDX-License-Identifier: Apache-2.0
//! The current process.

/// Ends the process with `code` (conductor sees it).
pub fn exit(code: i64) -> ! {
    crate::sys::process_exit(code)
}
