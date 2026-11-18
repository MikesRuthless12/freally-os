// SPDX-License-Identifier: Apache-2.0
//! The Rack's clock (TASK-059, FR-030): the owner's choice, 12-hour with no
//! seconds ("9:42 PM"). The time is the RTC's as the kernel reads it; a
//! machine that keeps its RTC in local time (as Windows does) shows local
//! time, and QEMU runs with `-rtc base=localtime` to match. Time zones come
//! with Settings.

use alloc::string::String;

/// The clock's words for `unix_ns`, 12-hour: "9:42 PM", "12:05 AM".
pub fn twelve_hour(unix_ns: u64) -> String {
    let secs = unix_ns / 1_000_000_000;
    let day = secs % 86_400;
    let (h, m) = ((day / 3600) as u32, (day / 60 % 60) as u32);
    let h12 = if h % 12 == 0 { 12 } else { h % 12 };
    alloc::format!("{h12}:{m:02} {}", if h < 12 { "AM" } else { "PM" })
}

/// Nanoseconds until the minute changes: the clock redraws then, and not
/// before (a still desktop costs nothing).
pub fn until_next_minute(unix_ns: u64) -> u64 {
    60_000_000_000 - unix_ns % 60_000_000_000
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u64 = 3600 * 1_000_000_000;
    const M: u64 = 60 * 1_000_000_000;

    #[test]
    fn twelve_hour_with_no_seconds() {
        assert_eq!(twelve_hour(0), "12:00 AM", "midnight");
        assert_eq!(twelve_hour(21 * H + 42 * M + 59_000_000_000), "9:42 PM");
        assert_eq!(twelve_hour(12 * H + 5 * M), "12:05 PM", "noon");
        assert_eq!(twelve_hour(9 * H + 7 * M), "9:07 AM");
        // 2026-11-18 11:59 PM UTC.
        assert_eq!(twelve_hour(1_795_046_340 * 1_000_000_000), "11:59 PM");
    }

    #[test]
    fn it_wakes_on_the_minute() {
        assert_eq!(until_next_minute(0), 60_000_000_000);
        assert_eq!(until_next_minute(59_999_999_999), 1);
        assert_eq!(until_next_minute(21 * H + 42 * M + 30_000_000_000), 30_000_000_000);
    }
}
