// SPDX-License-Identifier: Apache-2.0
//! The idle timer (TASK-077C, FR-091): after a while with no key or mouse
//! report, a screensaver starts; the next report ends it and does nothing
//! else. The default: Bounce after 10 minutes (the owner, 2026-11-18);
//! Settings ▸ Personalisation chooses from Phase 6.

/// Ten minutes.
pub const DEFAULT_AFTER_NS: u64 = 10 * 60 * 1_000_000_000;

#[derive(Debug)]
pub struct Idle {
    /// Which saver (`desktop::saver::Kind` as a number).
    pub kind: u8,
    /// How long idle before it starts (0: never).
    pub after_ns: u64,
    /// Reduce Motion: the still F instead.
    pub reduce_motion: bool,
    /// Whether one is up.
    pub on: bool,
    last_input: u64,
}

impl Idle {
    pub fn new(now: u64) -> Idle {
        Idle { kind: 0, after_ns: DEFAULT_AFTER_NS, reduce_motion: false, on: false, last_input: now }
    }

    /// A key or mouse report at `now`. True if it ended a saver: then it
    /// does nothing else.
    pub fn input(&mut self, now: u64) -> bool {
        self.last_input = now;
        core::mem::replace(&mut self.on, false)
    }

    /// When the saver is due, if one can start.
    pub fn deadline(&self) -> Option<u64> {
        (!self.on && self.after_ns > 0).then(|| self.last_input.saturating_add(self.after_ns))
    }

    /// True once: the saver's time has come (it's marked on).
    pub fn start_if_due(&mut self, now: u64) -> bool {
        if self.deadline().is_some_and(|d| now >= d) {
            self.on = true;
            return true;
        }
        false
    }

    /// A new choice: the count starts again from `now`.
    pub fn set(&mut self, kind: u8, after_ns: u64, now: u64) {
        (self.kind, self.after_ns, self.last_input) = (kind, after_ns, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 1_000_000_000;

    #[test]
    fn it_starts_when_idle_and_input_ends_it() {
        let mut i = Idle::new(0);
        assert_eq!(i.deadline(), Some(DEFAULT_AFTER_NS));
        i.set(2, 3 * S, 10 * S);
        assert!(!i.start_if_due(12 * S));
        // A key at 12 s puts it off to 15 s.
        assert!(!i.input(12 * S));
        assert!(!i.start_if_due(14 * S));
        assert!(i.start_if_due(15 * S));
        assert!(!i.start_if_due(16 * S), "once");
        assert_eq!(i.deadline(), None, "none while it's up");
        // The next report ends it (and does nothing else), and the count restarts.
        assert!(i.input(20 * S));
        assert!(!i.input(20 * S));
        assert_eq!(i.deadline(), Some(23 * S));
    }

    #[test]
    fn never_means_never() {
        let mut i = Idle::new(0);
        i.set(0, 0, 0);
        assert_eq!(i.deadline(), None);
        assert!(!i.start_if_due(u64::MAX));
    }
}
