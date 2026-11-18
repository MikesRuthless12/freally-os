// SPDX-License-Identifier: Apache-2.0
//! The global shortcuts (TASK-057, FR-028), seen before any window sees a
//! key:
//!
//! - `Super` pressed and let go alone: the launcher;
//! - `Super+M`: Monitor; `Super+T`: Terminal;
//! - `Alt+Tab` (and `Alt+Shift+Tab`): step through the windows, most recent
//!   first, while Alt is held; letting go of Alt keeps the one reached;
//! - `Alt+F4`: ask the focused window to close.
//!
//! A shortcut's keys never reach a window, their releases included, so no
//! window sees a key go up that it never saw go down.

use alloc::vec::Vec;

/// Key usages (HID), as drv-i8042 reports them.
pub mod usage {
    pub const M: u16 = 0x10;
    pub const T: u16 = 0x17;
    pub const TAB: u16 = 0x2B;
    pub const F4: u16 = 0x3D;
    pub const LEFT_ALT: u16 = 0xE2;
    pub const LEFT_META: u16 = 0xE3;
    pub const RIGHT_ALT: u16 = 0xE6;
    pub const RIGHT_META: u16 = 0xE7;
}

/// Modifier bits, as drv-i8042 reports them.
pub mod mods {
    pub const SHIFT: u8 = 1 << 0;
    pub const CTRL: u8 = 1 << 1;
    pub const ALT: u8 = 1 << 2;
    pub const META: u8 = 1 << 3;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Launcher,
    Monitor,
    Terminal,
    /// One step of Alt+Tab: forward, or back with Shift.
    Cycle {
        back: bool,
    },
    /// Alt let go after stepping: keep the window reached.
    CycleDone,
    CloseFocused,
}

/// What a key event comes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Not a shortcut: the focused window gets it.
    Pass,
    /// Part of a shortcut: no window sees it.
    Swallow,
    /// A shortcut fired (and no window sees the key).
    Fire(Action),
}

#[derive(Debug, Default)]
pub struct Shortcuts {
    /// Super went down and nothing else has been pressed since.
    super_alone: bool,
    /// Alt+Tab is stepping.
    cycling: bool,
    /// Keys whose press was a shortcut's: their release is swallowed too.
    held: Vec<u16>,
}

fn is_meta(u: u16) -> bool {
    u == usage::LEFT_META || u == usage::RIGHT_META
}

fn is_alt(u: u16) -> bool {
    u == usage::LEFT_ALT || u == usage::RIGHT_ALT
}

impl Shortcuts {
    /// One key, down or up, with the modifiers held after it.
    pub fn key(&mut self, u: u16, down: bool, m: u8) -> Outcome {
        if !down {
            if let Some(i) = self.held.iter().position(|&h| h == u) {
                self.held.swap_remove(i);
                if is_meta(u) && core::mem::take(&mut self.super_alone) {
                    return Outcome::Fire(Action::Launcher);
                }
                return Outcome::Swallow;
            }
            if is_alt(u) && self.cycling && m & mods::ALT == 0 {
                self.cycling = false;
                return Outcome::Fire(Action::CycleDone);
            }
            return Outcome::Pass;
        }
        if is_meta(u) {
            // Super is the system's: a window never sees it.
            self.super_alone = true;
            self.held.push(u);
            return Outcome::Swallow;
        }
        self.super_alone = false;
        let fired = if m & mods::META != 0 {
            match u {
                usage::M => Some(Action::Monitor),
                usage::T => Some(Action::Terminal),
                _ => None,
            }
        } else if m & mods::ALT != 0 && m & mods::CTRL == 0 {
            match u {
                usage::TAB => {
                    self.cycling = true;
                    Some(Action::Cycle { back: m & mods::SHIFT != 0 })
                }
                usage::F4 => Some(Action::CloseFocused),
                _ => None,
            }
        } else {
            None
        };
        match fired {
            Some(a) => {
                if !self.held.contains(&u) {
                    self.held.push(u);
                }
                Outcome::Fire(a)
            }
            None => Outcome::Pass,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALT: u8 = mods::ALT;
    const META: u8 = mods::META;

    #[test]
    fn super_alone_opens_the_launcher_on_release() {
        let mut s = Shortcuts::default();
        assert_eq!(s.key(usage::LEFT_META, true, META), Outcome::Swallow);
        assert_eq!(s.key(usage::LEFT_META, false, 0), Outcome::Fire(Action::Launcher));
        // Super with another key isn't the launcher.
        s.key(usage::LEFT_META, true, META);
        assert_eq!(s.key(usage::M, true, META), Outcome::Fire(Action::Monitor));
        assert_eq!(s.key(usage::M, false, META), Outcome::Swallow, "its release is swallowed too");
        assert_eq!(s.key(usage::LEFT_META, false, 0), Outcome::Swallow);
    }

    #[test]
    fn super_t_and_super_m() {
        let mut s = Shortcuts::default();
        s.key(usage::RIGHT_META, true, META);
        assert_eq!(s.key(usage::T, true, META), Outcome::Fire(Action::Terminal));
        assert_eq!(s.key(0x04, true, META), Outcome::Pass, "Super+A isn't ours");
    }

    #[test]
    fn alt_tab_steps_while_alt_is_held() {
        let mut s = Shortcuts::default();
        assert_eq!(s.key(usage::LEFT_ALT, true, ALT), Outcome::Pass, "Alt alone is the window's");
        assert_eq!(s.key(usage::TAB, true, ALT), Outcome::Fire(Action::Cycle { back: false }));
        assert_eq!(s.key(usage::TAB, false, ALT), Outcome::Swallow);
        assert_eq!(s.key(usage::TAB, true, ALT | mods::SHIFT), Outcome::Fire(Action::Cycle { back: true }));
        s.key(usage::TAB, false, ALT | mods::SHIFT);
        assert_eq!(s.key(usage::LEFT_ALT, false, 0), Outcome::Fire(Action::CycleDone));
        assert_eq!(s.key(usage::LEFT_ALT, true, ALT), Outcome::Pass);
        assert_eq!(s.key(usage::LEFT_ALT, false, 0), Outcome::Pass, "no stepping, nothing to keep");
    }

    #[test]
    fn alt_f4_closes_and_tab_alone_is_the_window_s() {
        let mut s = Shortcuts::default();
        assert_eq!(s.key(usage::F4, true, ALT), Outcome::Fire(Action::CloseFocused));
        assert_eq!(s.key(usage::F4, false, ALT), Outcome::Swallow);
        assert_eq!(s.key(usage::TAB, true, 0), Outcome::Pass);
        assert_eq!(s.key(usage::TAB, false, 0), Outcome::Pass);
        assert_eq!(s.key(usage::F4, true, ALT | mods::CTRL), Outcome::Pass, "Ctrl+Alt+F4 isn't ours");
    }
}
