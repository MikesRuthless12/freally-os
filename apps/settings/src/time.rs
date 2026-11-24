// SPDX-License-Identifier: Apache-2.0
//! Settings ▸ Time and language (TASK-180A, FR-080): the zone, and what
//! the computer's own clock (the RTC) keeps. Windows keeps that clock on
//! local time, so a machine that also runs Windows needs *Local time* here
//! for Freally OS to know the true time (and judge a certificate's dates
//! right). Saved machine-wide by conductor (`FreallyClock`, PRD § 3), not
//! per account; daylight saving is the person's to set, as the offset is now.

use crate::personalisation::Kind;
use alloc::string::String;
use alloc::vec::Vec;
pub use fbootctl::clock::Zone;

/// The offsets zones have (standard time and daylight saving), in minutes.
pub const OFFSETS: [i16; 40] = [
    -720, -660, -600, -570, -540, -480, -420, -360, -300, -240, -210, -180, -150, -120, -60, 0, 60, 120, 180, 210, 240,
    270, 300, 330, 345, 360, 390, 420, 480, 525, 540, 570, 600, 630, 660, 720, 765, 780, 825, 840,
];

/// The defaults (nothing kept): UTC, the RTC read as UTC.
pub fn defaults() -> Zone {
    Zone::default()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Zone,
    Rtc,
}

pub const ROWS: [Row; 2] = [Row::Zone, Row::Rtc];

impl Row {
    pub fn label(self) -> &'static str {
        match self {
            Row::Zone => "Time zone",
            Row::Rtc => "Hardware clock",
        }
    }

    /// The control's test id; an option's is this, `-`, and its number.
    pub fn id(self) -> &'static str {
        match self {
            Row::Zone => "settings-zone",
            Row::Rtc => "settings-rtc",
        }
    }

    pub fn kind(self) -> Kind {
        match self {
            Row::Zone => Kind::Stepper,
            Row::Rtc => Kind::Radio,
        }
    }

    pub fn options(self) -> Vec<String> {
        match self {
            Row::Zone => {
                OFFSETS.iter().map(|&m| alloc::format!("{}", Zone { offset_min: m, rtc_local: false }.name())).collect()
            }
            Row::Rtc => alloc::vec![String::from("Local time"), String::from("UTC")],
        }
    }

    pub fn chosen(self, z: &Zone) -> usize {
        match self {
            // A zone kept that isn't listed: the first listed past it.
            Row::Zone => OFFSETS.iter().position(|&m| m >= z.offset_min).unwrap_or(OFFSETS.len() - 1),
            Row::Rtc => usize::from(!z.rtc_local),
        }
    }

    /// The zone with option `i` chosen (`i` past the end: the last).
    pub fn set(self, z: &mut Zone, i: usize) {
        match self {
            Row::Zone => z.offset_min = OFFSETS[i.min(OFFSETS.len() - 1)],
            Row::Rtc => z.rtc_local = i == 0,
        }
    }
}

/// `18:05 UTC, 24 Nov 2026`: a time (ns since 1970) as the panel says it.
pub fn words(ns: u64, zone: &str) -> String {
    let secs = ns / 1_000_000_000;
    let (y, m, d) = app_session::import::date(secs);
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let day = secs % 86_400;
    alloc::format!("{:02}:{:02} {zone}, {d} {} {y}", day / 3600, day / 60 % 60, MONTHS[(m as usize + 11) % 12])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_offset_is_a_zone_in_order() {
        assert!(OFFSETS.windows(2).all(|w| w[0] < w[1]));
        assert!(OFFSETS.iter().all(|&m| Zone { offset_min: m, rtc_local: true }.valid()));
        assert_eq!(Row::Zone.options()[7], "UTC\u{2212}06:00");
        assert_eq!(Row::Zone.options()[15], "UTC");
        assert_eq!(Row::Zone.options()[24], "UTC+05:45");
    }

    #[test]
    fn each_row_reads_and_sets_its_part() {
        let mut z = defaults();
        assert_eq!((Row::Zone.chosen(&z), Row::Rtc.chosen(&z)), (15, 1), "UTC, the RTC on UTC");
        Row::Zone.set(&mut z, 7);
        Row::Rtc.set(&mut z, 0);
        assert_eq!(z, Zone { offset_min: -360, rtc_local: true });
        Row::Zone.set(&mut z, 99);
        assert_eq!(z.offset_min, 840, "past the end: the last");
        // A zone kept that isn't listed shows the next one listed.
        assert_eq!(Row::Zone.chosen(&Zone { offset_min: -15, rtc_local: false }), 15);
    }

    #[test]
    fn a_time_is_said_with_its_zone_and_date() {
        // 2026-11-24 18:05 UTC.
        assert_eq!(words(1_795_543_500 * 1_000_000_000, "UTC"), "18:05 UTC, 24 Nov 2026");
    }
}
