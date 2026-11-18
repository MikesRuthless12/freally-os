// SPDX-License-Identifier: Apache-2.0
//! The CMOS real-time clock (TASK-023), read once at boot as UTC.

use super::cpu;

fn cmos(reg: u8) -> u8 {
    cpu::outb(0x70, reg | 0x80); // keep NMIs masked while selecting
    cpu::inb(0x71)
}

fn updating() -> bool {
    cmos(0x0A) & 0x80 != 0
}

#[derive(PartialEq, Eq, Clone, Copy)]
struct Reading([u8; 7]);

fn reading() -> Reading {
    while updating() {}
    Reading([cmos(0x00), cmos(0x02), cmos(0x04), cmos(0x07), cmos(0x08), cmos(0x09), cmos(0x32)])
}

fn from_bcd(v: u8) -> u8 {
    (v & 0x0F) + (v >> 4) * 10
}

/// Days from 1970-01-01 to a civil date (proleptic Gregorian).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The RTC's time in Unix seconds (it's read twice until two readings agree).
pub fn read_unix() -> Option<u64> {
    let mut r = reading();
    loop {
        let again = reading();
        if again == r {
            break;
        }
        r = again;
    }
    let status_b = cmos(0x0B);
    let bcd = status_b & 0x04 == 0;
    let h24 = status_b & 0x02 != 0;
    let conv = |v: u8| if bcd { from_bcd(v) } else { v };
    let [s, mi, h, d, mo, y, c] = r.0;
    let pm = h & 0x80 != 0;
    let mut hour = conv(h & 0x7F) as u32;
    if !h24 {
        hour %= 12;
        if pm {
            hour += 12;
        }
    }
    let century = if c != 0 && c != 0xFF { conv(c) as i64 } else { 20 };
    let year = century * 100 + conv(y) as i64;
    let (month, day) = (conv(mo) as u32, conv(d) as u32);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour as i64 * 3600 + conv(mi) as i64 * 60 + conv(s) as i64;
    u64::try_from(secs).ok()
}

ktest!(rtc_civil_dates_convert, {
    if days_from_civil(1970, 1, 1) != 0
        || days_from_civil(2000, 3, 1) != 11_017
        || days_from_civil(2026, 11, 17) != 20_774
    {
        return Err("days_from_civil is wrong");
    }
    Ok(())
});
