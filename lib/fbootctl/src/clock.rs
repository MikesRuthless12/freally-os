// SPDX-License-Identifier: Apache-2.0
//! `FreallyClock` (PRD § 3, TASK-180A): the zone, and whether the computer's
//! clock (the CMOS RTC) keeps local time or UTC. A UEFI variable
//! (`FREALLY_VENDOR_GUID`, NV | BS | RT), 12 bytes: magic `FCK1`, the offset
//! in minutes (i16, east positive), the flags (bit 0: the RTC keeps local
//! time), a zero byte, and a CRC32C over the 8 bytes before it.

/// The variable's name.
pub const NAME: &str = "FreallyClock";
pub const SIZE: usize = 12;
pub const MAGIC: [u8; 4] = *b"FCK1";
/// The widest offsets a zone has: UTC−12:00 and UTC+14:00.
pub const MIN_OFFSET: i16 = -720;
pub const MAX_OFFSET: i16 = 840;
const RTC_LOCAL: u8 = 1;

/// The zone, and what the RTC keeps. The default (nothing kept): offset 0
/// and the RTC read as UTC, as every version before 0.9.0 read it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Zone {
    /// Minutes east of UTC: a multiple of 15 in `MIN_OFFSET..=MAX_OFFSET`.
    pub offset_min: i16,
    /// The RTC keeps local time (Windows' habit), not UTC.
    pub rtc_local: bool,
}

impl Zone {
    /// Whether the offset is one a zone can have.
    pub fn valid(&self) -> bool {
        (MIN_OFFSET..=MAX_OFFSET).contains(&self.offset_min) && self.offset_min % 15 == 0
    }

    /// The offset in seconds.
    pub fn offset_s(&self) -> i64 {
        self.offset_min as i64 * 60
    }

    /// What the RTC is ahead of UTC by, in seconds: the offset when it keeps
    /// local time, else nothing.
    pub fn rtc_ahead_s(&self) -> i64 {
        if self.rtc_local { self.offset_s() } else { 0 }
    }

    pub fn encode(&self) -> [u8; SIZE] {
        let mut b = [0u8; SIZE];
        b[..4].copy_from_slice(&MAGIC);
        b[4..6].copy_from_slice(&self.offset_min.to_le_bytes());
        b[6] = if self.rtc_local { RTC_LOCAL } else { 0 };
        let crc = fcrypto::crc32c::crc32c(&b[..8]);
        b[8..].copy_from_slice(&crc.to_le_bytes());
        b
    }

    /// The record, or `None` when it isn't one: the wrong size or magic, a
    /// flag or padding not zero, an offset no zone has, or a CRC that
    /// doesn't match.
    pub fn decode(b: &[u8]) -> Option<Zone> {
        if b.len() != SIZE || b[..4] != MAGIC || b[6] & !RTC_LOCAL != 0 || b[7] != 0 {
            return None;
        }
        if u32::from_le_bytes([b[8], b[9], b[10], b[11]]) != fcrypto::crc32c::crc32c(&b[..8]) {
            return None;
        }
        let z = Zone { offset_min: i16::from_le_bytes([b[4], b[5]]), rtc_local: b[6] & RTC_LOCAL != 0 };
        z.valid().then_some(z)
    }

    /// `UTC−06:00`, `UTC+05:30`, `UTC`.
    pub fn name(&self) -> ZoneName {
        ZoneName(self.offset_min)
    }
}

/// A zone's name, written without allocating (the kernel's log uses it).
pub struct ZoneName(i16);

impl core::fmt::Display for ZoneName {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.0 == 0 {
            return f.write_str("UTC");
        }
        let a = self.0.unsigned_abs();
        // U+2212, the minus sign, as the fleet writes it.
        write!(f, "UTC{}{:02}:{:02}", if self.0 < 0 { '\u{2212}' } else { '+' }, a / 60, a % 60)
    }
}
