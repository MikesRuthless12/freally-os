// SPDX-License-Identifier: Apache-2.0
//! finput: keyboards and mice, decoded (TASK-043; no_std, no allocation).
//!
//! **Freally key codes are USB HID usage IDs** (usage page 7): a key's code
//! names the key, not the character, whatever the layout. `Set2` turns PS/2
//! scan code set 2 bytes into key events (extended keys, Print Screen's fake
//! shifts and Pause's eight-byte sequence included); `us_char` gives the US
//! layout's character; `Mouse` turns PS/2 mouse packets into motion, buttons
//! and the IntelliMouse wheel, in HID's directions (y down, wheel up positive).
#![no_std]

/// A key went down or up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyEvent {
    pub usage: u16,
    pub down: bool,
}

/// Some HID usages used by name.
pub mod usage {
    pub const A: u16 = 0x04;
    pub const ENTER: u16 = 0x28;
    pub const CAPS_LOCK: u16 = 0x39;
    pub const PRINT_SCREEN: u16 = 0x46;
    pub const PAUSE: u16 = 0x48;
    pub const LEFT_CTRL: u16 = 0xE0;
    pub const LEFT_SHIFT: u16 = 0xE1;
    pub const LEFT_ALT: u16 = 0xE2;
    pub const LEFT_META: u16 = 0xE3;
    pub const RIGHT_CTRL: u16 = 0xE4;
    pub const RIGHT_SHIFT: u16 = 0xE5;
    pub const RIGHT_ALT: u16 = 0xE6;
    pub const RIGHT_META: u16 = 0xE7;
}

/// Set 2 make codes (not extended) → HID usages.
const PLAIN: &[(u8, u16)] = &[
    (0x1C, 0x04),
    (0x32, 0x05),
    (0x21, 0x06),
    (0x23, 0x07),
    (0x24, 0x08),
    (0x2B, 0x09),
    (0x34, 0x0A),
    (0x33, 0x0B),
    (0x43, 0x0C),
    (0x3B, 0x0D),
    (0x42, 0x0E),
    (0x4B, 0x0F),
    (0x3A, 0x10),
    (0x31, 0x11),
    (0x44, 0x12),
    (0x4D, 0x13),
    (0x15, 0x14),
    (0x2D, 0x15),
    (0x1B, 0x16),
    (0x2C, 0x17),
    (0x3C, 0x18),
    (0x2A, 0x19),
    (0x1D, 0x1A),
    (0x22, 0x1B),
    (0x35, 0x1C),
    (0x1A, 0x1D), // A–Z
    (0x16, 0x1E),
    (0x1E, 0x1F),
    (0x26, 0x20),
    (0x25, 0x21),
    (0x2E, 0x22),
    (0x36, 0x23),
    (0x3D, 0x24),
    (0x3E, 0x25),
    (0x46, 0x26),
    (0x45, 0x27), // 1–0
    (0x5A, 0x28),
    (0x76, 0x29),
    (0x66, 0x2A),
    (0x0D, 0x2B),
    (0x29, 0x2C),
    (0x4E, 0x2D),
    (0x55, 0x2E),
    (0x54, 0x2F),
    (0x5B, 0x30),
    (0x5D, 0x31),
    (0x4C, 0x33),
    (0x52, 0x34),
    (0x0E, 0x35),
    (0x41, 0x36),
    (0x49, 0x37),
    (0x4A, 0x38),
    (0x58, 0x39), // Enter … Caps Lock
    (0x05, 0x3A),
    (0x06, 0x3B),
    (0x04, 0x3C),
    (0x0C, 0x3D),
    (0x03, 0x3E),
    (0x0B, 0x3F),
    (0x83, 0x40),
    (0x0A, 0x41),
    (0x01, 0x42),
    (0x09, 0x43),
    (0x78, 0x44),
    (0x07, 0x45), // F1–F12
    (0x7E, 0x47),
    (0x77, 0x53),
    (0x7C, 0x55),
    (0x7B, 0x56),
    (0x79, 0x57),
    (0x69, 0x59),
    (0x72, 0x5A),
    (0x7A, 0x5B),
    (0x6B, 0x5C),
    (0x73, 0x5D),
    (0x74, 0x5E),
    (0x6C, 0x5F),
    (0x75, 0x60),
    (0x7D, 0x61),
    (0x70, 0x62),
    (0x71, 0x63),
    (0x61, 0x64), // Scroll Lock, Num Lock, the keypad, the ISO key
    (0x12, 0xE1),
    (0x59, 0xE5),
    (0x14, 0xE0),
    (0x11, 0xE2), // shifts, left Ctrl, left Alt
];

/// Set 2 make codes after 0xE0 → HID usages.
const EXTENDED: &[(u8, u16)] = &[
    (0x14, 0xE4),
    (0x11, 0xE6),
    (0x1F, 0xE3),
    (0x27, 0xE7),
    (0x2F, 0x65),
    (0x70, 0x49),
    (0x6C, 0x4A),
    (0x7D, 0x4B),
    (0x71, 0x4C),
    (0x69, 0x4D),
    (0x7A, 0x4E),
    (0x74, 0x4F),
    (0x6B, 0x50),
    (0x72, 0x51),
    (0x75, 0x52),
    (0x4A, 0x54),
    (0x5A, 0x58),
    (0x7C, 0x46),
];

fn lookup(table: &[(u8, u16)], code: u8) -> Option<u16> {
    table.iter().find(|(c, _)| *c == code).map(|(_, u)| *u)
}

/// The scan code set 2 state machine.
#[derive(Default)]
pub struct Set2 {
    extended: bool,
    released: bool,
    /// Bytes of Pause's sequence (E1 14 77 E1 F0 14 F0 77) seen so far.
    pause: u8,
    /// Pause's release, owed after its press.
    pause_up: bool,
    /// The last code no key has: (after 0xE0, the code).
    unknown: Option<(bool, u8)>,
}

/// Pause has no break code: one sequence is a press and a release.
const PAUSE_SEQ: [u8; 8] = [0xE1, 0x14, 0x77, 0xE1, 0xF0, 0x14, 0xF0, 0x77];

impl Set2 {
    /// Pause has no release code: after its press, the release it owes.
    /// Call after every `feed`.
    pub fn pending(&mut self) -> Option<KeyEvent> {
        core::mem::take(&mut self.pause_up).then_some(KeyEvent { usage: usage::PAUSE, down: false })
    }

    /// A code no key has, seen since the last call: (after 0xE0, the code).
    pub fn take_unknown(&mut self) -> Option<(bool, u8)> {
        self.unknown.take()
    }

    /// Feeds one byte; returns the key event it completes, if any.
    pub fn feed(&mut self, b: u8) -> Option<KeyEvent> {
        if self.pause > 0 || (b == 0xE1 && !self.extended && !self.released) {
            if b == PAUSE_SEQ[self.pause as usize] {
                self.pause += 1;
                if self.pause as usize == PAUSE_SEQ.len() {
                    self.pause = 0;
                    self.pause_up = true;
                    return Some(KeyEvent { usage: usage::PAUSE, down: true });
                }
            } else {
                self.pause = 0;
            }
            return None;
        }
        match b {
            0xE0 => {
                self.extended = true;
                None
            }
            0xF0 => {
                self.released = true;
                None
            }
            // Self-test passed, echo, acknowledgements, errors: not keys.
            0xAA | 0xEE | 0xFA | 0xFE | 0x00 | 0xFF => {
                self.extended = false;
                self.released = false;
                None
            }
            code => {
                let (ext, up) = (self.extended, self.released);
                self.extended = false;
                self.released = false;
                // Print Screen's fake left shift (E0 12 / E0 F0 12) isn't a key.
                if ext && code == 0x12 {
                    return None;
                }
                let usage = if ext { lookup(EXTENDED, code) } else { lookup(PLAIN, code) };
                if usage.is_none() {
                    self.unknown = Some((ext, code));
                }
                Some(KeyEvent { usage: usage?, down: !up })
            }
        }
    }
}

/// Scan code set 1 → set 2, for each set 1 make code below 0x80 (0: no key).
/// It undoes the 8042's translation byte by byte: a keyboard that won't
/// leave set 2 behind a controller that won't stop translating (laptop
/// embedded controllers do both: TASK-072) still reads right. Extended keys
/// (after 0xE0) use the same second byte, so this one table covers them.
const SET1_TO_SET2: [u8; 128] = {
    let mut t = [0u8; 128];
    let pairs: [(u8, u8); 98] = [
        (0x01, 0x76),
        (0x02, 0x16),
        (0x03, 0x1E),
        (0x04, 0x26),
        (0x05, 0x25),
        (0x06, 0x2E),
        (0x07, 0x36),
        (0x08, 0x3D),
        (0x09, 0x3E),
        (0x0A, 0x46),
        (0x0B, 0x45),
        (0x0C, 0x4E),
        (0x0D, 0x55),
        (0x0E, 0x66),
        (0x0F, 0x0D),
        (0x10, 0x15),
        (0x11, 0x1D),
        (0x12, 0x24),
        (0x13, 0x2D),
        (0x14, 0x2C),
        (0x15, 0x35),
        (0x16, 0x3C),
        (0x17, 0x43),
        (0x18, 0x44),
        (0x19, 0x4D),
        (0x1A, 0x54),
        (0x1B, 0x5B),
        (0x1C, 0x5A),
        (0x1D, 0x14),
        (0x1E, 0x1C),
        (0x1F, 0x1B),
        (0x20, 0x23),
        (0x21, 0x2B),
        (0x22, 0x34),
        (0x23, 0x33),
        (0x24, 0x3B),
        (0x25, 0x42),
        (0x26, 0x4B),
        (0x27, 0x4C),
        (0x28, 0x52),
        (0x29, 0x0E),
        (0x2A, 0x12),
        (0x2B, 0x5D),
        (0x2C, 0x1A),
        (0x2D, 0x22),
        (0x2E, 0x21),
        (0x2F, 0x2A),
        (0x30, 0x32),
        (0x31, 0x31),
        (0x32, 0x3A),
        (0x33, 0x41),
        (0x34, 0x49),
        (0x35, 0x4A),
        (0x36, 0x59),
        (0x37, 0x7C),
        (0x38, 0x11),
        (0x39, 0x29),
        (0x3A, 0x58),
        (0x3B, 0x05),
        (0x3C, 0x06),
        (0x3D, 0x04),
        (0x3E, 0x0C),
        (0x3F, 0x03),
        (0x40, 0x0B),
        (0x41, 0x83),
        (0x42, 0x0A),
        (0x43, 0x01),
        (0x44, 0x09),
        (0x45, 0x77),
        (0x46, 0x7E),
        (0x47, 0x6C),
        (0x48, 0x75),
        (0x49, 0x7D),
        (0x4A, 0x7B),
        (0x4B, 0x6B),
        (0x4C, 0x73),
        (0x4D, 0x74),
        (0x4E, 0x79),
        (0x4F, 0x69),
        (0x50, 0x72),
        (0x51, 0x7A),
        (0x52, 0x70),
        (0x53, 0x71),
        (0x54, 0x84),
        (0x56, 0x61),
        (0x57, 0x78),
        (0x58, 0x07),
        (0x5B, 0x1F),
        (0x5C, 0x27),
        (0x5D, 0x2F),
        (0x5E, 0x37),
        (0x5F, 0x3F),
        (0x63, 0x5E),
        (0x70, 0x13),
        (0x73, 0x51),
        (0x79, 0x64),
        (0x7B, 0x67),
        (0x7D, 0x6A),
    ];
    let mut i = 0;
    while i < pairs.len() {
        t[pairs[i].0 as usize] = pairs[i].1;
        i += 1;
    }
    t
};

/// Scan code set 1 (what a translating 8042 hands over), read by turning
/// each byte back into set 2 and feeding [`Set2`]: one decoder, one key table.
#[derive(Default)]
pub struct Set1 {
    set2: Set2,
}

impl Set1 {
    /// Feeds one set 1 byte; returns the key events it completes (a break
    /// becomes two set 2 bytes, so at most two).
    pub fn feed(&mut self, b: u8) -> [Option<KeyEvent>; 2] {
        match b {
            // Prefixes and the keyboard's own answers pass through. Not 0xAA:
            // in set 1 that's Left Shift's release (its self-test byte only
            // comes during setup, read there).
            0xE0 | 0xE1 | 0xEE | 0xFA | 0xFE | 0x00 | 0xFF => [self.set2.feed(b), None],
            b => {
                let code = SET1_TO_SET2[(b & 0x7F) as usize];
                if code == 0 {
                    // No key: as Set2 would say of a code it doesn't know.
                    self.set2.unknown = Some((false, b));
                    return [None, None];
                }
                if b & 0x80 != 0 { [self.set2.feed(0xF0), self.set2.feed(code)] } else { [self.set2.feed(code), None] }
            }
        }
    }

    /// As [`Set2::pending`].
    pub fn pending(&mut self) -> Option<KeyEvent> {
        self.set2.pending()
    }

    /// As [`Set2::take_unknown`].
    pub fn take_unknown(&mut self) -> Option<(bool, u8)> {
        self.set2.take_unknown()
    }
}

/// The modifier keys held, and Caps Lock.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
    pub caps: bool,
}

impl Modifiers {
    /// Follows an event (Caps Lock toggles on its press).
    pub fn update(&mut self, e: KeyEvent) {
        match e.usage {
            usage::LEFT_SHIFT | usage::RIGHT_SHIFT => self.shift = e.down,
            usage::LEFT_CTRL | usage::RIGHT_CTRL => self.ctrl = e.down,
            usage::LEFT_ALT | usage::RIGHT_ALT => self.alt = e.down,
            usage::LEFT_META | usage::RIGHT_META => self.meta = e.down,
            usage::CAPS_LOCK if e.down => self.caps = !self.caps,
            _ => {}
        }
    }

    pub fn bits(&self) -> u8 {
        self.shift as u8
            | (self.ctrl as u8) << 1
            | (self.alt as u8) << 2
            | (self.meta as u8) << 3
            | (self.caps as u8) << 4
    }
}

/// The US layout: the character a key types, if any.
pub fn us_char(usage: u16, m: Modifiers) -> Option<char> {
    if (0x04..=0x1D).contains(&usage) {
        let c = (b'a' + (usage - 0x04) as u8) as char;
        return Some(if m.shift ^ m.caps { c.to_ascii_uppercase() } else { c });
    }
    let (plain, shifted) = match usage {
        0x1E => ('1', '!'),
        0x1F => ('2', '@'),
        0x20 => ('3', '#'),
        0x21 => ('4', '$'),
        0x22 => ('5', '%'),
        0x23 => ('6', '^'),
        0x24 => ('7', '&'),
        0x25 => ('8', '*'),
        0x26 => ('9', '('),
        0x27 => ('0', ')'),
        0x2C => (' ', ' '),
        0x2D => ('-', '_'),
        0x2E => ('=', '+'),
        0x2F => ('[', '{'),
        0x30 => (']', '}'),
        0x31 => ('\\', '|'),
        0x33 => (';', ':'),
        0x34 => ('\'', '"'),
        0x35 => ('`', '~'),
        0x36 => (',', '<'),
        0x37 => ('.', '>'),
        0x38 => ('/', '?'),
        _ => return None,
    };
    Some(if m.shift { shifted } else { plain })
}

/// One mouse report, in HID's directions (as Freally key codes are HID's).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct MouseEvent {
    /// Right is positive.
    pub dx: i16,
    /// Down is positive (PS/2 reports up as positive: flipped).
    pub dy: i16,
    /// Notches away from the user (scrolling up) are positive (flipped too).
    pub wheel: i8,
    /// Bit 0 left, 1 right, 2 middle.
    pub buttons: u8,
}

/// PS/2 mouse packets: 3 bytes, or 4 with the IntelliMouse wheel.
pub struct Mouse {
    wheel: bool,
    buf: [u8; 4],
    n: usize,
}

impl Mouse {
    pub fn new(wheel: bool) -> Mouse {
        Mouse { wheel, buf: [0; 4], n: 0 }
    }

    pub fn feed(&mut self, b: u8) -> Option<MouseEvent> {
        // The first byte always has bit 3 set: anything else means we lost sync.
        if self.n == 0 && b & 0x08 == 0 {
            return None;
        }
        self.buf[self.n] = b;
        self.n += 1;
        if self.n < if self.wheel { 4 } else { 3 } {
            return None;
        }
        self.n = 0;
        let f = self.buf[0];
        // Overflowed motion is unreliable: drop the packet.
        if f & 0xC0 != 0 {
            return None;
        }
        let dx = self.buf[1] as i16 - if f & 0x10 != 0 { 256 } else { 0 };
        let up = self.buf[2] as i16 - if f & 0x20 != 0 { 256 } else { 0 };
        // The wheel's four bits, signed; PS/2 counts towards the user.
        let towards = if self.wheel { ((self.buf[3] << 4) as i8) >> 4 } else { 0 };
        Some(MouseEvent { dx, dy: -up, wheel: -towards, buttons: f & 7 })
    }
}

#[cfg(test)]
mod tests;
