// SPDX-License-Identifier: Apache-2.0
//! A/B slots (TASK-045, FR-003): reads `FreallyBootCtl`, picks the slot by
//! fbootctl's rules (PRD § 3), reads that slot's image, and saves the record
//! before anything else happens when the rules changed it, so a trial that
//! crashes still counts.

use crate::read;
use crate::uefi::*;
use fbootctl::{BootCtl, State, slot_name};
use fcore_abi::FREALLY_VENDOR_GUID;

/// Each slot's image, in the files layout.
const SYSTEM: [&[u16]; 2] = [read::SYSTEM_A, read::SYSTEM_B];

/// `FreallyBootCtl` as NUL-terminated UTF-16.
const NAME16: [u16; 15] = {
    let b = fbootctl::NAME.as_bytes();
    let mut out = [0u16; 15];
    let mut i = 0;
    while i < b.len() {
        out[i] = b[i] as u16;
        i += 1;
    }
    out
};

pub struct Chosen {
    pub slot: u8,
    pub image: Result<&'static [u8], &'static str>,
    /// The record as saved.
    pub ctl: BootCtl,
}

/// The record as the firmware holds it, if it's there and fits.
///
/// # Safety
/// `rt` is the firmware's runtime services table.
unsafe fn read_var(rt: *const RuntimeServices, buf: &mut [u8; fbootctl::SIZE]) -> Option<&[u8]> {
    let (mut attrs, mut size) = (0u32, buf.len());
    // SAFETY: the firmware's GetVariable, with a buffer of `size` bytes.
    let st = unsafe {
        ((*rt).get_variable)(NAME16.as_ptr(), &FREALLY_VENDOR_GUID, &mut attrs, &mut size, buf.as_mut_ptr().cast())
    };
    (!is_error(st)).then_some(&buf[..size])
}

/// Saves the record. A firmware that refuses is said, not fatal: the boot goes on.
///
/// # Safety
/// As `read_var`.
pub unsafe fn save(rt: *const RuntimeServices, ctl: &BootCtl) {
    let b = ctl.encode();
    // SAFETY: the firmware's SetVariable, with the record's 32 bytes.
    let st = unsafe {
        ((*rt).set_variable)(NAME16.as_ptr(), &FREALLY_VENDOR_GUID, fbootctl::ATTRIBUTES, b.len(), b.as_ptr().cast())
    };
    if is_error(st) {
        say!("freally-boot: couldn't save FreallyBootCtl (status {st:#x})");
    }
}

/// Picks the slot and reads its image. `Err` when neither slot can start,
/// with the record as kept, for the screen.
///
/// # Safety
/// Boot services are running; `st`, `bs` and `image` are the firmware's.
pub unsafe fn choose(st: &SystemTable, bs: &BootServices, image: Handle) -> Result<Chosen, BootCtl> {
    let rt = st.runtime_services;
    let mut buf = [0u8; fbootctl::SIZE];
    // SAFETY: as this function's.
    let stored = unsafe { read_var(rt, &mut buf) };
    let mut present = [true, true];
    loop {
        let d = fbootctl::decide(stored, present, "");
        let Some(s) = d.boot else {
            say!(
                "freally-boot: ERROR neither slot can start (A {}, B {})",
                d.ctl.slot[0].state.name(),
                d.ctl.slot[1].state.name()
            );
            if d.changed {
                // SAFETY: as this function's.
                unsafe { save(rt, &d.ctl) };
            }
            return Err(d.ctl);
        };
        // SAFETY: as this function's.
        let bytes = unsafe { read::read_image(bs, image, SYSTEM[s as usize]) };
        if bytes == Err(read::MISSING) {
            say!("freally-boot: slot {} has no image", slot_name(s));
            present[s as usize] = false;
            continue;
        }
        if d.rebuilt {
            say!("freally-boot: FreallyBootCtl was missing or damaged: rebuilt as A Good, B Empty");
        }
        let other = 1 - s;
        let slot = &d.ctl.slot[s as usize];
        if d.fell_back {
            say!(
                "freally-boot: slot {} can't start ({}); booting slot {}",
                slot_name(other),
                d.ctl.slot[other as usize].state.name(),
                slot_name(s)
            );
        } else if slot.state == State::Trial {
            say!("freally-boot: booting slot {} (trial, {} tries left)", slot_name(s), slot.tries_left);
        } else {
            say!("freally-boot: booting slot {}", slot_name(s));
        }
        if d.changed {
            // SAFETY: as this function's.
            unsafe { save(rt, &d.ctl) };
        }
        return Ok(Chosen { slot: s, image: bytes, ctl: d.ctl });
    }
}

/// One line of text, for the screen.
pub struct Line {
    buf: [u8; 160],
    len: usize,
}

impl Line {
    pub fn new() -> Line {
        Line { buf: [0; 160], len: 0 }
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl core::fmt::Write for Line {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let n = s.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

/// "Slot A: Bad (0.4.0). Slot B: Empty."
pub fn states(ctl: &BootCtl) -> Line {
    let mut l = Line::new();
    for s in 0..2u8 {
        let slot = &ctl.slot[s as usize];
        let _ = core::fmt::write(&mut l, format_args!("Slot {}: {}", slot_name(s), slot.state.name()));
        if !slot.version().is_empty() {
            let _ = core::fmt::write(&mut l, format_args!(" ({})", slot.version()));
        }
        let _ = core::fmt::write(&mut l, format_args!("{}", if s == 0 { ". " } else { "." }));
    }
    l
}
