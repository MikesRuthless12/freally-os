// SPDX-License-Identifier: Apache-2.0
//! What real controllers do that QEMU's doesn't (TASK-072), and what
//! drv-i8042 does about each:
//!
//! - **A self-test that doesn't answer 0x55,** or a port test that isn't 0,
//!   though the keyboard works (embedded controllers emulate the 8042, some
//!   loosely): a warning, and carry on; only a keyboard that won't answer a
//!   reset fails.
//! - **A keyboard that won't leave its own set** (refuses `F0 02`): the
//!   controller's translation is turned back on, and keys are read as set 1.
//! - **Firmware that keeps translation on** however it's asked (the keyboard
//!   took set 2, but says `0x41` when asked which set: set 2 seen through
//!   translation): keys are read as set 1.
//! - **Slow controllers:** every wait is generous (a second for a reset or a
//!   self-test), and a reset that isn't acknowledged is tried once more.
//! - **No mouse port** (the OMEN's touchpad is I2C, not PS/2): the keyboard
//!   alone, said in the log.

use alloc::vec::Vec;
use finput::KeyEvent;

/// How the keyboard's bytes will arrive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    /// Set 2, as the keyboard sends it (translation off).
    Set2,
    /// Set 1, translated by the controller; `turn_on` when the driver must
    /// turn translation back on itself (the keyboard kept its own set).
    TranslatedSet1 { turn_on: bool },
}

/// The scheme, from whether the keyboard took set 2 (`F0 02` acknowledged)
/// and what it said when asked which set it uses (`F0 00`: 0x02 is set 2 as
/// sent, 0x41 is set 2 seen through translation, no answer is a keyboard
/// that doesn't say).
pub fn plan(took_set2: bool, reported: Option<u8>) -> Scheme {
    match (took_set2, reported) {
        (false, _) => Scheme::TranslatedSet1 { turn_on: true },
        (true, Some(0x41)) => Scheme::TranslatedSet1 { turn_on: false },
        // 0x02, or no answer: set 2 as configured.
        (true, _) => Scheme::Set2,
    }
}

/// The line the log gets for a scheme.
pub fn describe(s: Scheme) -> &'static str {
    match s {
        Scheme::Set2 => "keyboard on set 2",
        Scheme::TranslatedSet1 { turn_on: true } => {
            "keyboard keeps its own scan codes: read as set 1, through the controller's translation"
        }
        Scheme::TranslatedSet1 { turn_on: false } => {
            "the firmware keeps the controller translating: keyboard read as set 1"
        }
    }
}

/// The keyboard's bytes into key events, by its scheme.
pub enum Decode {
    Set2(finput::Set2),
    Set1(finput::Set1),
}

impl Decode {
    pub fn new(s: Scheme) -> Decode {
        match s {
            Scheme::Set2 => Decode::Set2(finput::Set2::default()),
            Scheme::TranslatedSet1 { .. } => Decode::Set1(finput::Set1::default()),
        }
    }

    /// The key events one byte completes (Pause's owed release included).
    pub fn feed(&mut self, b: u8) -> Vec<KeyEvent> {
        let mut out = Vec::new();
        match self {
            Decode::Set2(s) => {
                out.extend(s.feed(b));
                out.extend(s.pending());
            }
            Decode::Set1(s) => {
                out.extend(s.feed(b).into_iter().flatten());
                out.extend(s.pending());
            }
        }
        out
    }

    /// A code no key has, seen since the last call.
    pub fn take_unknown(&mut self) -> Option<(bool, u8)> {
        match self {
            Decode::Set2(s) => s.take_unknown(),
            Decode::Set1(s) => s.take_unknown(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scheme_follows_what_the_keyboard_said() {
        assert_eq!(plan(true, Some(0x02)), Scheme::Set2);
        // A keyboard that doesn't say which set it uses: set 2, as asked.
        assert_eq!(plan(true, None), Scheme::Set2);
        // Set 2 seen through translation the firmware keeps on.
        assert_eq!(plan(true, Some(0x41)), Scheme::TranslatedSet1 { turn_on: false });
        // A keyboard that keeps its own set: translation back on, set 1.
        assert_eq!(plan(false, None), Scheme::TranslatedSet1 { turn_on: true });
    }

    #[test]
    fn both_schemes_type_the_same_key() {
        // "A" pressed and released: set 2's 1C F0 1C, set 1's 1E 9E.
        let mut two = Decode::new(Scheme::Set2);
        let mut one = Decode::new(Scheme::TranslatedSet1 { turn_on: true });
        let a: Vec<KeyEvent> = [0x1C, 0xF0, 0x1C].iter().flat_map(|&b| two.feed(b)).collect();
        let b: Vec<KeyEvent> = [0x1E, 0x9E].iter().flat_map(|&b| one.feed(b)).collect();
        assert_eq!(a.len(), 2);
        assert_eq!(a, b);
    }
}
