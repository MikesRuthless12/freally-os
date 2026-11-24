// SPDX-License-Identifier: Apache-2.0
//! fbootctl: `FreallyBootCtl`, the A/B slot record (PRD § 3, FR-003), and the
//! rules the bootloader and conductor follow with it (TASK-045). no_std, no
//! allocation: the bootloader runs it before the kernel exists.
//!
//! The record is a UEFI variable (`FREALLY_VENDOR_GUID`, NV | BS | RT), 32
//! bytes: magic `FBC1`, the active slot, three bytes of padding, each slot's
//! state, tries left and version (8 bytes, NUL-padded), and a CRC32C over
//! everything before it.
#![no_std]

pub mod clock;

/// The variable's name.
pub const NAME: &str = "FreallyBootCtl";
/// Non-volatile, boot-services and runtime access.
pub const ATTRIBUTES: u32 = 0x7;
pub const SIZE: usize = 32;
pub const MAGIC: [u8; 4] = *b"FBC1";
/// The tries a new trial gets.
pub const TRIES: u8 = 3;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum State {
    Empty = 0,
    Good = 1,
    Trial = 2,
    Bad = 3,
}

impl State {
    fn from_u8(v: u8) -> Option<State> {
        Some(match v {
            0 => State::Empty,
            1 => State::Good,
            2 => State::Trial,
            3 => State::Bad,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            State::Empty => "Empty",
            State::Good => "Good",
            State::Trial => "Trial",
            State::Bad => "Bad",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Slot {
    pub state: State,
    pub tries_left: u8,
    /// The version it holds, NUL-padded ("0.8.1").
    pub version: [u8; 8],
}

impl Slot {
    pub const EMPTY: Slot = Slot { state: State::Empty, tries_left: 0, version: [0; 8] };

    /// The version as text.
    pub fn version(&self) -> &str {
        let n = self.version.iter().position(|&b| b == 0).unwrap_or(8);
        core::str::from_utf8(&self.version[..n]).unwrap_or("?")
    }
}

/// A version string, NUL-padded to 8 bytes (cut if longer).
pub fn version_bytes(v: &str) -> [u8; 8] {
    let mut out = [0u8; 8];
    let n = v.len().min(8);
    out[..n].copy_from_slice(&v.as_bytes()[..n]);
    out
}

/// "A" or "B".
pub fn slot_name(s: u8) -> &'static str {
    if s == 0 { "A" } else { "B" }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BootCtl {
    /// 0 = A, 1 = B.
    pub active: u8,
    pub slot: [Slot; 2],
}

impl BootCtl {
    /// What a missing or corrupt record is rebuilt as: A Good, B Empty.
    pub fn fresh(version_a: &str) -> BootCtl {
        BootCtl {
            active: 0,
            slot: [Slot { state: State::Good, tries_left: 0, version: version_bytes(version_a) }, Slot::EMPTY],
        }
    }

    pub fn encode(&self) -> [u8; SIZE] {
        let mut b = [0u8; SIZE];
        b[..4].copy_from_slice(&MAGIC);
        b[4] = self.active;
        for (i, s) in self.slot.iter().enumerate() {
            let at = 8 + i * 10;
            b[at] = s.state as u8;
            b[at + 1] = s.tries_left;
            b[at + 2..at + 10].copy_from_slice(&s.version);
        }
        let crc = fcrypto::crc32c::crc32c(&b[..28]);
        b[28..].copy_from_slice(&crc.to_le_bytes());
        b
    }

    /// The record, if it's whole: the right size, magic and CRC, and every
    /// field in range.
    pub fn decode(b: &[u8]) -> Option<BootCtl> {
        if b.len() != SIZE || b[..4] != MAGIC || b[4] > 1 || b[5..8] != [0, 0, 0] {
            return None;
        }
        if u32::from_le_bytes([b[28], b[29], b[30], b[31]]) != fcrypto::crc32c::crc32c(&b[..28]) {
            return None;
        }
        let slot = |i: usize| -> Option<Slot> {
            let at = 8 + i * 10;
            let mut version = [0u8; 8];
            version.copy_from_slice(&b[at + 2..at + 10]);
            Some(Slot { state: State::from_u8(b[at])?, tries_left: b[at + 1], version })
        };
        Some(BootCtl { active: b[4], slot: [slot(0)?, slot(1)?] })
    }

    /// Conductor: an update in slot `s` (the e2e door, later Settings ▸
    /// Updates): a trial of `version` with three tries, made active.
    pub fn start_trial(&mut self, s: u8, version: &str) {
        self.slot[s as usize] = Slot { state: State::Trial, tries_left: TRIES, version: version_bytes(version) };
        self.active = s;
    }

    /// Conductor: slot `s` started cleanly. True if that changed the record
    /// (a trial became Good).
    pub fn mark_good(&mut self, s: u8) -> bool {
        let slot = &mut self.slot[s as usize];
        if slot.state != State::Trial {
            return false;
        }
        slot.state = State::Good;
        slot.tries_left = 0;
        true
    }

    /// What the user is told about this boot, if anything: the other slot
    /// holds an update that didn't start (FR-003's acceptance).
    pub fn fell_back_from(&self, booted: u8) -> Option<(&str, &str)> {
        let other = &self.slot[1 - booted as usize];
        (other.state == State::Bad).then(|| (other.version(), self.slot[booted as usize].version()))
    }
}

/// The bootloader's choice for this boot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Decision {
    /// The slot to boot; None if neither can (the panic screen, with both states).
    pub boot: Option<u8>,
    /// The record to keep (written back when `changed`).
    pub ctl: BootCtl,
    pub changed: bool,
    /// The stored record was missing or corrupt, and was rebuilt.
    pub rebuilt: bool,
    /// The active slot couldn't boot; this boot is the other one.
    pub fell_back: bool,
}

/// The bootloader's rules (PRD § 3): `stored` is the variable as read (None
/// if missing), `present` whether each slot's image is on the medium, and
/// `version_a` what a rebuilt record says slot A holds.
pub fn decide(stored: Option<&[u8]>, present: [bool; 2], version_a: &str) -> Decision {
    let decoded = stored.and_then(BootCtl::decode);
    let rebuilt = decoded.is_none();
    let mut ctl = decoded.unwrap_or_else(|| BootCtl::fresh(version_a));
    let mut changed = rebuilt;
    let s = ctl.active;
    let slot = &mut ctl.slot[s as usize];
    let bootable = match slot.state {
        // A trial found at 0 used all its tries: it's Bad now.
        State::Trial if slot.tries_left == 0 => {
            slot.state = State::Bad;
            changed = true;
            false
        }
        State::Trial if present[s as usize] => {
            slot.tries_left -= 1;
            changed = true;
            true
        }
        State::Good => present[s as usize],
        _ => false,
    };
    if bootable {
        return Decision { boot: Some(s), ctl, changed, rebuilt, fell_back: false };
    }
    let other = 1 - s;
    if ctl.slot[other as usize].state == State::Good && present[other as usize] {
        ctl.active = other;
        return Decision { boot: Some(other), ctl, changed: true, rebuilt, fell_back: true };
    }
    Decision { boot: None, ctl, changed, rebuilt, fell_back: false }
}

#[cfg(test)]
mod tests;
