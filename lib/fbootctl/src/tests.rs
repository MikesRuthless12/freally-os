// SPDX-License-Identifier: Apache-2.0
//! The record's format and every rule of PRD § 3.

use super::*;

const BOTH: [bool; 2] = [true, true];

fn trial_b(tries: u8) -> BootCtl {
    let mut c = BootCtl::fresh("0.8.0");
    c.start_trial(1, "0.8.1");
    c.slot[1].tries_left = tries;
    c
}

#[test]
fn the_record_round_trips_and_is_32_bytes() {
    let c = trial_b(2);
    let b = c.encode();
    assert_eq!(b.len(), 32);
    assert_eq!(&b[..4], b"FBC1");
    assert_eq!(BootCtl::decode(&b), Some(c));
    assert_eq!(c.slot[1].version(), "0.8.1");
}

#[test]
fn a_damaged_record_is_refused() {
    let good = trial_b(2).encode();
    for i in 0..32 {
        let mut b = good;
        b[i] ^= 0x10;
        assert_eq!(BootCtl::decode(&b), None, "byte {i} flipped was accepted");
    }
    assert_eq!(BootCtl::decode(&good[..31]), None);
}

#[test]
fn a_missing_or_corrupt_record_is_rebuilt_as_a_good_b_empty() {
    for stored in [None, Some(&[0u8; 32][..]), Some(&b"FBC1"[..])] {
        let d = decide(stored, BOTH, "0.4.0");
        assert!(d.rebuilt && d.changed);
        assert_eq!(d.boot, Some(0));
        assert_eq!(d.ctl, BootCtl::fresh("0.4.0"));
        assert_eq!(d.ctl.slot[1].state, State::Empty);
    }
}

#[test]
fn a_good_active_slot_boots_and_nothing_is_written() {
    let stored = BootCtl::fresh("0.4.0").encode();
    let d = decide(Some(&stored), BOTH, "0.4.0");
    assert_eq!((d.boot, d.changed, d.rebuilt, d.fell_back), (Some(0), false, false, false));
}

#[test]
fn a_trial_crashing_three_times_is_left_on_the_fourth_boot() {
    let mut stored = trial_b(TRIES).encode();
    for left in [2, 1, 0] {
        let d = decide(Some(&stored), BOTH, "0.8.0");
        assert_eq!(d.boot, Some(1), "a trial with tries left boots");
        assert!(d.changed && !d.fell_back);
        assert_eq!(d.ctl.slot[1].tries_left, left);
        stored = d.ctl.encode(); // ... and crashes before conductor marks it Good.
    }
    let d = decide(Some(&stored), BOTH, "0.8.0");
    assert_eq!(d.boot, Some(0));
    assert!(d.fell_back && d.changed);
    assert_eq!(d.ctl.active, 0);
    assert_eq!(d.ctl.slot[1].state, State::Bad);
    assert_eq!(d.ctl.fell_back_from(0), Some(("0.8.1", "0.8.0")));
}

#[test]
fn a_trial_marked_good_stays() {
    let mut c = trial_b(TRIES);
    let d = decide(Some(&c.encode()), BOTH, "0.8.0");
    assert_eq!(d.boot, Some(1));
    c = d.ctl;
    assert!(c.mark_good(1));
    assert!(!c.mark_good(1), "a second mark changes nothing");
    let d = decide(Some(&c.encode()), BOTH, "0.8.0");
    assert_eq!((d.boot, d.changed), (Some(1), false));
    assert_eq!(c.fell_back_from(1), None);
}

#[test]
fn a_missing_active_image_falls_back_without_marking_it() {
    let stored = trial_b(TRIES).encode();
    let d = decide(Some(&stored), [true, false], "0.8.0");
    assert_eq!(d.boot, Some(0));
    assert!(d.fell_back);
    assert_eq!(d.ctl.slot[1].state, State::Trial, "a missing file isn't a failed trial");
}

#[test]
fn neither_bootable_is_said_and_both_states_kept() {
    let mut c = BootCtl::fresh("0.8.0");
    c.slot[0].state = State::Bad;
    let d = decide(Some(&c.encode()), BOTH, "0.8.0");
    assert_eq!(d.boot, None);
    // A Good slot whose image is gone, and nothing else: nothing to boot either.
    let d = decide(Some(&BootCtl::fresh("0.8.0").encode()), [false, false], "0.8.0");
    assert_eq!(d.boot, None);
}

#[test]
fn the_clock_record_round_trips_and_damage_is_refused() {
    use crate::clock::{SIZE, Zone};
    for z in [
        Zone { offset_min: -360, rtc_local: true },
        Zone { offset_min: 330, rtc_local: false },
        Zone { offset_min: -720, rtc_local: true },
        Zone { offset_min: 840, rtc_local: false },
        Zone::default(),
    ] {
        let b = z.encode();
        assert_eq!(b.len(), SIZE);
        assert_eq!(&b[..4], b"FCK1");
        assert_eq!(Zone::decode(&b), Some(z));
        // Every bit flipped, one at a time: refused.
        for i in 0..SIZE * 8 {
            let mut d = b;
            d[i / 8] ^= 1 << (i % 8);
            assert_eq!(Zone::decode(&d), None, "bit {i} of {z:?}");
        }
        assert_eq!(Zone::decode(&b[..SIZE - 1]), None);
    }
    // Offsets no zone has, even with a right CRC.
    for off in [-735, 855, 7, -1] {
        assert_eq!(Zone::decode(&Zone { offset_min: off, rtc_local: false }.encode()), None, "{off}");
    }
}

#[test]
fn the_rtc_is_ahead_of_utc_only_when_it_keeps_local_time() {
    use crate::clock::Zone;
    assert_eq!(Zone { offset_min: -360, rtc_local: true }.rtc_ahead_s(), -21_600);
    assert_eq!(Zone { offset_min: -360, rtc_local: false }.rtc_ahead_s(), 0);
    assert_eq!(Zone { offset_min: 345, rtc_local: true }.rtc_ahead_s(), 20_700);
}

#[test]
fn zones_are_named_as_the_fleet_writes_them() {
    use crate::clock::Zone;
    extern crate std;
    use std::format;
    assert_eq!(format!("{}", Zone { offset_min: -360, rtc_local: true }.name()), "UTC\u{2212}06:00");
    assert_eq!(format!("{}", Zone { offset_min: 330, rtc_local: false }.name()), "UTC+05:30");
    assert_eq!(format!("{}", Zone::default().name()), "UTC");
}
