// SPDX-License-Identifier: Apache-2.0
//! The A/B record, as conductor keeps it (TASK-045, FR-003).
//!
//! - It says so when this boot fell back from an update that didn't start
//!   (the slot the bootloader left `Bad`).
//! - It marks a trial slot `Good` once the system has been up for 10 s. "Up"
//!   is the desktop input-ready from Phase 3 (S4); until the desktop exists,
//!   every boot service running (S2).
//! - Writes go through `uefi_set_var` (the boot-control grant); a firmware
//!   that refuses leaves the trial as it was, and says so.

use fbootctl::{BootCtl, State, slot_name};
use fcore_abi::FErr;
use fstd::time::{Duration, Instant};

/// How long the system is up before a trial counts as started cleanly.
const GOOD_AFTER: Duration = Duration::from_secs(10);

pub struct Boot {
    /// The slot this boot came from (0 = A, 1 = B).
    pub slot: u8,
    /// The record as the bootloader left it, then as conductor wrote it.
    pub ctl: Option<BootCtl>,
    /// When a trial slot becomes Good (ns), once the system is up.
    good_at: Option<u64>,
    announced: bool,
}

fn write(ctl: &BootCtl) -> Result<(), FErr> {
    fstd::sys::uefi_set_var(fbootctl::NAME, &ctl.encode())
}

impl Boot {
    pub fn new(slot: u8, bootctl: &[u8; 32]) -> Boot {
        Boot { slot, ctl: BootCtl::decode(bootctl), good_at: None, announced: false }
    }

    /// What this boot is, on the log, and a fallback in the user's words:
    /// once, when the logger first takes conductor's lines.
    pub fn announce(&mut self) {
        if core::mem::replace(&mut self.announced, true) {
            return;
        }
        let Some(ctl) = &self.ctl else {
            fstd::warn!("conductor: started from slot {} (no A/B record)", slot_name(self.slot));
            return;
        };
        let me = &ctl.slot[self.slot as usize];
        fstd::info!("conductor: started from slot {} ({}, {})", slot_name(self.slot), me.state.name(), me.version());
        if let Some((failed, now)) = ctl.fell_back_from(self.slot) {
            fstd::warn!("conductor: The update to {failed} didn't start, so Freally OS went back to {now}.");
        }
    }

    /// The system is up: a trial starts its 10 s.
    pub fn up(&mut self) {
        if self.good_at.is_none() && self.ctl.is_some_and(|c| c.slot[self.slot as usize].state == State::Trial) {
            self.good_at = Some(Instant::after(GOOD_AFTER));
        }
    }

    /// When conductor next has something to do here.
    pub fn deadline(&self) -> Option<u64> {
        self.good_at
    }

    /// Marks the trial Good when its time comes.
    pub fn tick(&mut self) {
        if self.good_at.is_none_or(|t| Instant::now().as_ns() < t) {
            return;
        }
        self.good_at = None;
        let Some(mut ctl) = self.ctl else { return };
        if !ctl.mark_good(self.slot) {
            return;
        }
        match write(&ctl) {
            Ok(()) => {
                self.ctl = Some(ctl);
                fstd::info!(
                    "conductor: slot {} ({}) started cleanly: marked Good",
                    slot_name(self.slot),
                    ctl.slot[self.slot as usize].version()
                );
            }
            Err(e) => fstd::error!("conductor: Couldn't record that this version started cleanly. ({})", e.describe()),
        }
    }

    /// The next boot from the other slot, if it holds a system that started
    /// cleanly (the Brain's `reboot { slot: "other" }`, dev images; TASK-084).
    #[cfg(any(feature = "flavour-dev", test))]
    #[cfg_attr(not(feature = "flavour-dev"), allow(dead_code))]
    pub fn switch_to_other(&mut self) -> Result<(), alloc::string::String> {
        let other = 1 - self.slot;
        let mut ctl = self.ctl.ok_or_else(|| alloc::string::String::from("there's no A/B record to change"))?;
        let s = &ctl.slot[other as usize];
        if s.state != State::Good {
            return Err(alloc::format!(
                "slot {} holds no system that started cleanly ({})",
                slot_name(other),
                s.state.name()
            ));
        }
        ctl.active = other;
        write(&ctl).map_err(|e| alloc::format!("the firmware refused the A/B record ({})", e.describe()))?;
        self.ctl = Some(ctl);
        Ok(())
    }

    /// An update in slot `s`: a trial of `version`, made active for the next
    /// boot. Its one caller today is the e2e door, so it's compiled into e2e
    /// images only; Settings ▸ Updates (v0.8.0) takes the gate off.
    #[cfg(feature = "flavour-e2e")]
    pub fn start_trial(&mut self, s: u8, version: &str) -> Result<(), FErr> {
        let mut ctl = self.ctl.ok_or(FErr::BadState)?;
        ctl.start_trial(s, version);
        write(&ctl)?;
        self.ctl = Some(ctl);
        fstd::info!("conductor: slot {} is on trial ({version}) from the next boot", slot_name(s));
        Ok(())
    }
}
