// SPDX-License-Identifier: Apache-2.0
//! The decoders against the sequences a PS/2 keyboard and mouse send.

extern crate std;

use super::*;
use std::vec::Vec;

fn keys(bytes: &[u8]) -> Vec<KeyEvent> {
    let mut d = Set2::default();
    let mut out = Vec::new();
    for &b in bytes {
        out.extend(d.feed(b));
        out.extend(d.pending());
    }
    out
}

fn down(usage: u16) -> KeyEvent {
    KeyEvent { usage, down: true }
}
fn up(usage: u16) -> KeyEvent {
    KeyEvent { usage, down: false }
}

#[test]
fn plain_keys_go_down_and_up() {
    assert_eq!(keys(&[0x1C, 0xF0, 0x1C]), [down(usage::A), up(usage::A)]);
    assert_eq!(keys(&[0x5A, 0xF0, 0x5A]), [down(usage::ENTER), up(usage::ENTER)]);
    assert_eq!(
        keys(&[0x12, 0x1C, 0xF0, 0x1C, 0xF0, 0x12]),
        [down(usage::LEFT_SHIFT), down(usage::A), up(usage::A), up(usage::LEFT_SHIFT)]
    );
}

#[test]
fn extended_keys_are_their_own_keys() {
    // Right arrow; the right Ctrl (not the left one); the keypad's Enter.
    assert_eq!(keys(&[0xE0, 0x74, 0xE0, 0xF0, 0x74]), [down(0x4F), up(0x4F)]);
    assert_eq!(keys(&[0xE0, 0x14, 0xE0, 0xF0, 0x14]), [down(usage::RIGHT_CTRL), up(usage::RIGHT_CTRL)]);
    assert_eq!(keys(&[0xE0, 0x5A]), [down(0x58)]);
}

#[test]
fn print_screen_and_pause() {
    // Print Screen wraps itself in a fake shift; only the key is reported.
    assert_eq!(
        keys(&[0xE0, 0x12, 0xE0, 0x7C, 0xE0, 0xF0, 0x7C, 0xE0, 0xF0, 0x12]),
        [down(usage::PRINT_SCREEN), up(usage::PRINT_SCREEN)]
    );
    // Pause: eight bytes and no release code: a press, then the release it owes.
    assert_eq!(keys(&[0xE1, 0x14, 0x77, 0xE1, 0xF0, 0x14, 0xF0, 0x77]), [down(usage::PAUSE), up(usage::PAUSE)]);
    // A broken Pause sequence is dropped, and what follows still decodes.
    assert_eq!(keys(&[0xE1, 0x14, 0x1C]), []);
    assert_eq!(keys(&[0xE1, 0x14, 0x00, 0x1C]), [down(usage::A)]);
}

#[test]
fn replies_and_unknown_codes_are_not_keys() {
    assert_eq!(keys(&[0xFA, 0xAA, 0xEE, 0x02, 0x1C]), [down(usage::A)]);
    // An unknown code is told once, for the driver's Debug line; replies aren't codes.
    let mut d = Set2::default();
    for b in [0xFA, 0x02] {
        d.feed(b);
    }
    assert_eq!(d.take_unknown(), Some((false, 0x02)));
    assert_eq!(d.take_unknown(), None);
    for b in [0xE0, 0xF0, 0x99] {
        d.feed(b);
    }
    assert_eq!(d.take_unknown(), Some((true, 0x99)));
}

#[test]
fn the_us_layout_types_every_printable_character() {
    let mut typed = std::string::String::new();
    // Every printable ASCII character, as the keys and shift that make it.
    for u in (0x04..=0x38u16).chain([0x2C]) {
        for shift in [false, true] {
            if let Some(c) = us_char(u, Modifiers { shift, ..Modifiers::default() }) {
                typed.push(c);
            }
        }
    }
    for c in ' '..='~' {
        assert!(typed.contains(c), "the US layout can't type {c:?}");
    }
    // Caps Lock: letters only, and shift undoes it.
    let caps = Modifiers { caps: true, ..Modifiers::default() };
    assert_eq!(us_char(usage::A, caps), Some('A'));
    assert_eq!(us_char(usage::A, Modifiers { shift: true, caps: true, ..Modifiers::default() }), Some('a'));
    assert_eq!(us_char(0x1E, caps), Some('1'));
}

#[test]
fn modifiers_follow_the_keys() {
    let mut m = Modifiers::default();
    m.update(down(usage::RIGHT_SHIFT));
    m.update(down(usage::LEFT_CTRL));
    assert!(m.shift && m.ctrl && !m.alt);
    m.update(up(usage::RIGHT_SHIFT));
    m.update(down(usage::CAPS_LOCK));
    m.update(up(usage::CAPS_LOCK));
    assert!(!m.shift && m.caps);
    assert_eq!(m.bits(), 0b10010);
}

#[test]
fn mouse_packets_move_click_and_scroll() {
    let mut m = Mouse::new(true);
    // Left button; 3 left, 5 up, one notch away from the user (PS/2's -1,
    // four bits signed): HID's directions are y down and the wheel up positive.
    let e: Vec<MouseEvent> = [0x09 | 0x10, 0xFD, 0x05, 0x0F].iter().filter_map(|&b| m.feed(b)).collect();
    assert_eq!(e, [MouseEvent { dx: -3, dy: -5, wheel: 1, buttons: 1 }]);
    // Out of sync (bit 3 clear in a first byte): skipped until a real first byte.
    let mut m = Mouse::new(false);
    let e: Vec<MouseEvent> = [0x00, 0x08, 0x01, 0x02].iter().filter_map(|&b| m.feed(b)).collect();
    assert_eq!(e, [MouseEvent { dx: 1, dy: -2, wheel: 0, buttons: 0 }]);
    // Overflow: dropped.
    let mut m = Mouse::new(false);
    assert_eq!([0x48, 0x10, 0x10].iter().filter_map(|&b| m.feed(b)).count(), 0);
}
