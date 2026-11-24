// SPDX-License-Identifier: Apache-2.0
//! What the bootloader allocated for Freally Core, by address (TASK-113).
//!
//! Every page is allocated as the firmware's own `LoaderData`, never as an
//! OS-defined memory type (0x8000_0000 and up), though the UEFI spec allows
//! them: older EDK2 firmware, VirtualBox 7.2's among them, indexes a table
//! by each region's type inside `ExitBootServices` and faults on one. So the
//! bootloader notes here which pages are the kernel's, the image's and the
//! boot data's (by their `memtype` tag), and the final memory map's
//! `LoaderData` regions are split by this ledger.

use core::cell::UnsafeCell;

/// The most allocations it remembers.
pub const MAX: usize = 32;
const PAGE: u64 = 4096;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Entry {
    start: u64,
    pages: u64,
    tag: u32,
}

#[derive(Debug)]
pub struct Ledger {
    entries: [Entry; MAX],
    n: usize,
}

impl Ledger {
    pub const fn new() -> Ledger {
        Ledger { entries: [Entry { start: 0, pages: 0, tag: 0 }; MAX], n: 0 }
    }

    /// Notes `pages` pages at `start` as `tag`'s; false when it's full.
    pub fn note(&mut self, start: u64, pages: u64, tag: u32) -> bool {
        if self.n == MAX {
            return false;
        }
        self.entries[self.n] = Entry { start, pages, tag };
        self.n += 1;
        true
    }

    /// The pieces of the firmware's region of `pages` pages at `start`, in
    /// address order: each run a noted allocation covers, with its tag, and
    /// each run none covers (`None`).
    pub fn split(&self, start: u64, pages: u64, mut each: impl FnMut(u64, u64, Option<u32>)) {
        let end = start + pages * PAGE;
        let mut at = start;
        while at < end {
            let next = self.entries[..self.n]
                .iter()
                .filter(|e| e.pages > 0 && e.start + e.pages * PAGE > at && e.start < end)
                .min_by_key(|e| e.start.max(at));
            let Some(e) = next else {
                each(at, (end - at) / PAGE, None);
                return;
            };
            let from = e.start.max(at);
            if from > at {
                each(at, (from - at) / PAGE, None);
            }
            let to = (e.start + e.pages * PAGE).min(end);
            each(from, (to - from) / PAGE, Some(e.tag));
            at = to;
        }
    }
}

struct Global(UnsafeCell<Ledger>);

// SAFETY: the bootloader runs on one CPU, and nothing here is re-entered.
unsafe impl Sync for Global {}

static LEDGER: Global = Global(UnsafeCell::new(Ledger::new()));

/// Notes an allocation in the bootloader's ledger; false when it's full.
pub fn note(start: u64, pages: u64, tag: u32) -> bool {
    // SAFETY: one CPU, no re-entry (see `Global`).
    unsafe { (*LEDGER.0.get()).note(start, pages, tag) }
}

/// [`Ledger::split`] over the bootloader's ledger.
pub fn split(start: u64, pages: u64, each: impl FnMut(u64, u64, Option<u32>)) {
    // SAFETY: as above.
    unsafe { (*LEDGER.0.get()).split(start, pages, each) }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    fn pieces(l: &Ledger, start: u64, pages: u64) -> Vec<(u64, u64, Option<u32>)> {
        let mut v = Vec::new();
        l.split(start, pages, |s, p, t| v.push((s, p, t)));
        v
    }

    #[test]
    fn a_region_splits_around_what_was_noted() {
        let mut l = Ledger::new();
        // Firmware region: 16 pages at 0x10_0000; two allocations inside it,
        // noted out of order, and one elsewhere.
        assert!(l.note(0x10_8000, 2, 0x8000_0002));
        assert!(l.note(0x10_2000, 3, 0x8000_0001));
        assert!(l.note(0x90_0000, 8, 0x8000_0003));
        assert_eq!(
            pieces(&l, 0x10_0000, 16),
            [
                (0x10_0000, 2, None),
                (0x10_2000, 3, Some(0x8000_0001)),
                (0x10_5000, 3, None),
                (0x10_8000, 2, Some(0x8000_0002)),
                (0x10_A000, 6, None),
            ]
        );
        // Covered whole; touching allocations; none at all.
        assert_eq!(pieces(&l, 0x90_0000, 8), [(0x90_0000, 8, Some(0x8000_0003))]);
        let mut t = Ledger::new();
        t.note(0x2000, 1, 1);
        t.note(0x3000, 1, 2);
        assert_eq!(pieces(&t, 0x2000, 2), [(0x2000, 1, Some(1)), (0x3000, 1, Some(2))]);
        assert_eq!(pieces(&Ledger::new(), 0x2000, 4), [(0x2000, 4, None)]);
        // An allocation running past the region is cut at its end.
        assert_eq!(pieces(&t, 0x1000, 2), [(0x1000, 1, None), (0x2000, 1, Some(1))]);
    }

    #[test]
    fn the_ledger_has_a_limit() {
        let mut l = Ledger::new();
        for i in 0..MAX as u64 {
            assert!(l.note(i * PAGE, 1, 7));
        }
        assert!(!l.note(0xF000_0000, 1, 7), "the 33rd is refused, never dropped silently");
    }
}
