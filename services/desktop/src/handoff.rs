// SPDX-License-Identifier: Apache-2.0
//! What the desktop hands the next build in a swap (TASK-085, FR-018; dev
//! images): the launcher, open or not, what was typed in it and the row
//! chosen. The Brain's demo swaps the desktop with the launcher open, and
//! the new build opens it again as it was.

use alloc::string::String;
use alloc::vec::Vec;

fwire::wire_struct! {
    /// The handed-over state. `version` first: a build that reads an older
    /// or newer layout starts fresh instead of misreading it.
    pub struct State { pub version: u8, pub launcher_open: bool, pub query: String, pub selected: u32 }
}

/// This layout.
pub const VERSION: u8 = 1;

impl State {
    pub fn encode(&self) -> Vec<u8> {
        fwire::to_payload(self)
    }

    /// The state, if it's this layout.
    pub fn decode(bytes: &[u8]) -> Option<State> {
        fwire::from_payload::<State>(bytes).ok().filter(|s| s.version == VERSION)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_launcher_survives_a_swap() {
        let s = State { version: VERSION, launcher_open: true, query: String::from("mon"), selected: 1 };
        assert_eq!(State::decode(&s.encode()), Some(s.clone()));
        // Another layout, or bytes that aren't one: a fresh start.
        let other = State { version: VERSION + 1, ..s };
        assert_eq!(State::decode(&other.encode()), None);
        assert_eq!(State::decode(b"\x01\x02"), None);
    }
}
