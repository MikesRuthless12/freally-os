// SPDX-License-Identifier: Apache-2.0
//! Settings' two panes (TASK-104, FR-048): the categories down the leading
//! side as a vertical tab list (↑ ↓ cycle, Home and End, one tab stop), the
//! chosen category's controls beside them, and the fleet's bar at the foot
//! (the rule of 2026-11-17): **Restore Defaults** on the left; **OK**,
//! **Cancel**, **Apply** on the right.
//!
//! Nothing is saved until Apply or OK; Apply is lit only while something
//! changed. Escape, the window's ×, or a click outside is Cancel, and what
//! was shown live (the theme, the background) goes back. Restore Defaults
//! resets the open panel; a panel with unsaved changes carries a dot.

use crate::key;
use crate::personalisation::{self as pers, Kind, ROWS};
use crate::time::{self, Zone};
use fwire::proto::stage::Look;

/// The categories, in their fixed order (FR-080: the list grows to
/// Windows', each name and place fixed now). Update and the hardware report
/// joined between Accounts and About in Phase 7 (TASK-110, TASK-111); Time
/// and language between Accounts and Update in Phase 8 (TASK-180A).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    Personalisation,
    Accounts,
    Time,
    Update,
    Hardware,
    About,
}

pub const PANELS: [Panel; 6] =
    [Panel::Personalisation, Panel::Accounts, Panel::Time, Panel::Update, Panel::Hardware, Panel::About];

impl Panel {
    pub fn name(self) -> &'static str {
        match self {
            Panel::Personalisation => "Personalisation",
            Panel::Accounts => "Accounts",
            Panel::Time => "Time and language",
            Panel::Update => "Update",
            Panel::Hardware => "Hardware report",
            Panel::About => "About",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Panel::Personalisation => "settings-tab-personalisation",
            Panel::Accounts => "settings-tab-accounts",
            Panel::Time => "settings-tab-time",
            Panel::Update => "settings-tab-update",
            Panel::Hardware => "settings-tab-hardware",
            Panel::About => "settings-tab-about",
        }
    }
}

/// The bar's keys, as drawn: Restore Defaults on the left, then OK, Cancel
/// and Apply on the right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bar {
    Defaults,
    Ok,
    Cancel,
    Apply,
}

pub const BAR: [Bar; 4] = [Bar::Defaults, Bar::Ok, Bar::Cancel, Bar::Apply];

impl Bar {
    pub fn label(self) -> &'static str {
        match self {
            Bar::Defaults => "Restore Defaults",
            Bar::Ok => "OK",
            Bar::Cancel => "Cancel",
            Bar::Apply => "Apply",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Bar::Defaults => "settings-defaults",
            Bar::Ok => "settings-ok",
            Bar::Cancel => "settings-cancel",
            Bar::Apply => "settings-apply",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Tabs,
    /// The open panel's control `i` (a Personalisation or Time row, an Accounts key).
    Control(usize),
    Bar(Bar),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Draw again.
    Changed,
    /// The pending look changed: draw again, and show what's live.
    Previewed,
    /// Save the pending look.
    Apply,
    /// Save it (if it changed) and close.
    Ok,
    /// Put back what was shown live, and close.
    Cancel,
    /// The Accounts panel's key `i` was pressed.
    Action(usize),
    Nothing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shell {
    pub panel: Panel,
    /// The look as it's stored (and on the screen, but for what's live).
    pub saved: Look,
    /// The look as chosen here.
    pub pending: Look,
    pub focus: Focus,
    /// How many keys the Accounts panel has (the app counts them).
    pub actions: usize,
    /// How many keys the Update panel has (the app counts them).
    pub updates: usize,
    /// The zone as the kernel has it, and as chosen here (the machine's,
    /// not the account's).
    pub zone_saved: Zone,
    pub zone_pending: Zone,
}

impl Shell {
    pub fn new(saved: Look, zone: Zone) -> Shell {
        Shell {
            panel: Panel::Personalisation,
            pending: saved.clone(),
            saved,
            focus: Focus::Tabs,
            actions: 0,
            updates: 0,
            zone_saved: zone,
            zone_pending: zone,
        }
    }

    pub fn dirty(&self) -> bool {
        self.pending != self.saved || self.zone_pending != self.zone_saved
    }

    /// Whether `p` carries the dot: it has changes not yet saved.
    pub fn panel_dirty(&self, p: Panel) -> bool {
        match p {
            Panel::Personalisation => self.pending != self.saved,
            Panel::Time => self.zone_pending != self.zone_saved,
            _ => false,
        }
    }

    /// A row of choices on the open panel: its kind, the option chosen, how
    /// many options; `None` on a panel of keys.
    pub fn row(&self, i: usize) -> Option<(Kind, usize, usize)> {
        match self.panel {
            Panel::Personalisation => ROWS.get(i).map(|r| (r.kind(), r.chosen(&self.pending), r.options().len())),
            Panel::Time => time::ROWS.get(i).map(|r| (r.kind(), r.chosen(&self.zone_pending), r.options().len())),
            _ => None,
        }
    }

    /// Whether a bar key can be pressed: Apply only while something
    /// changed; Restore Defaults only where it would change something.
    pub fn enabled(&self, b: Bar) -> bool {
        match b {
            Bar::Apply => self.dirty(),
            Bar::Defaults => match self.panel {
                Panel::Personalisation => self.pending != pers::defaults(),
                Panel::Time => self.zone_pending != time::defaults(),
                _ => false,
            },
            Bar::Ok | Bar::Cancel => true,
        }
    }

    /// The panel's controls that take the focus.
    fn controls(&self) -> alloc::vec::Vec<usize> {
        match self.panel {
            Panel::Personalisation => (0..ROWS.len()).filter(|&i| ROWS[i].enabled(&self.pending)).collect(),
            Panel::Accounts => (0..self.actions).collect(),
            Panel::Time => (0..time::ROWS.len()).collect(),
            Panel::Update => (0..self.updates).collect(),
            Panel::Hardware => (0..crate::hardware::KEYS.len()).collect(),
            Panel::About => alloc::vec::Vec::new(),
        }
    }

    /// Every tab stop, in order: the tab list (one stop), the panel's
    /// controls, the bar's keys that can be pressed.
    pub fn stops(&self) -> alloc::vec::Vec<Focus> {
        let mut v = alloc::vec![Focus::Tabs];
        v.extend(self.controls().into_iter().map(Focus::Control));
        v.extend(BAR.into_iter().filter(|&b| self.enabled(b)).map(Focus::Bar));
        v
    }

    fn tab(&mut self, back: bool) -> Outcome {
        let stops = self.stops();
        let at = stops.iter().position(|&f| f == self.focus).unwrap_or(0);
        let n = stops.len();
        self.focus = stops[if back { (at + n - 1) % n } else { (at + 1) % n }];
        Outcome::Changed
    }

    /// Another category. The focus stays on the tab list.
    pub fn show(&mut self, p: Panel) -> Outcome {
        if self.panel == p {
            return Outcome::Nothing;
        }
        self.panel = p;
        Outcome::Changed
    }

    /// A row's option `i` chosen, on the panel open.
    pub fn choose(&mut self, row: usize, i: usize) -> Outcome {
        if self.panel == Panel::Time {
            let Some(&r) = time::ROWS.get(row) else { return Outcome::Nothing };
            let was = self.zone_pending;
            r.set(&mut self.zone_pending, i);
            // Nothing is shown live: the zone is the kernel's only once saved.
            return if self.zone_pending == was { Outcome::Nothing } else { Outcome::Changed };
        }
        let Some(&r) = ROWS.get(row) else { return Outcome::Nothing };
        if !r.enabled(&self.pending) {
            return Outcome::Nothing;
        }
        let was = self.pending.clone();
        r.set(&mut self.pending, i);
        // A focused idle time gone with the screensaver: the focus moves on.
        if !crate::personalisation::Row::Idle.enabled(&self.pending) && self.focus == Focus::Control(ROWS.len() - 1) {
            self.focus = Focus::Control(row);
        }
        if self.pending == was { Outcome::Nothing } else { Outcome::Previewed }
    }

    /// A bar key pressed.
    pub fn press(&mut self, b: Bar) -> Outcome {
        if !self.enabled(b) {
            return Outcome::Nothing;
        }
        match b {
            Bar::Defaults if self.panel == Panel::Time => {
                self.zone_pending = time::defaults();
                if self.focus == Focus::Bar(Bar::Defaults) {
                    self.focus = Focus::Bar(Bar::Ok);
                }
                Outcome::Changed
            }
            Bar::Defaults => {
                self.pending = pers::defaults();
                if self.focus == Focus::Bar(Bar::Defaults) {
                    self.focus = Focus::Bar(Bar::Ok);
                }
                Outcome::Previewed
            }
            Bar::Ok => Outcome::Ok,
            Bar::Cancel => Outcome::Cancel,
            Bar::Apply => Outcome::Apply,
        }
    }

    /// Saved: what's chosen is what's stored. Apply's focus moves to OK.
    pub fn saved(&mut self) {
        self.saved = self.pending.clone();
        self.zone_saved = self.zone_pending;
        if self.focus == Focus::Bar(Bar::Apply) {
            self.focus = Focus::Bar(Bar::Ok);
        }
    }

    /// Cancelled: what's chosen goes back to what's stored.
    pub fn cancel(&mut self) {
        self.pending = self.saved.clone();
        self.zone_pending = self.zone_saved;
    }

    pub fn key(&mut self, usage: u16, shift: bool) -> Outcome {
        match usage {
            key::TAB => return self.tab(shift),
            key::ESCAPE => return Outcome::Cancel,
            _ => {}
        }
        match self.focus {
            Focus::Tabs => {
                let at = PANELS.iter().position(|&p| p == self.panel).unwrap_or(0);
                let n = PANELS.len();
                let to = match usage {
                    key::UP => (at + n - 1) % n,
                    key::DOWN => (at + 1) % n,
                    key::HOME => 0,
                    key::END => n - 1,
                    _ => return Outcome::Nothing,
                };
                self.show(PANELS[to])
            }
            Focus::Control(i) if self.row(i).is_some() => {
                let Some((kind, at, n)) = self.row(i) else { return Outcome::Nothing };
                let to = match (kind, usage) {
                    (Kind::Switch, key::SPACE | key::ENTER) => 1 - at,
                    (Kind::Radio, key::LEFT | key::UP) => (at + n - 1) % n,
                    (Kind::Radio, key::RIGHT | key::DOWN) => (at + 1) % n,
                    (Kind::Stepper, key::LEFT | key::DOWN) => at.saturating_sub(1),
                    (Kind::Stepper, key::RIGHT | key::UP) => (at + 1).min(n - 1),
                    (_, key::HOME) => 0,
                    (_, key::END) => n - 1,
                    _ => return Outcome::Nothing,
                };
                self.choose(i, to)
            }
            Focus::Control(i) => match usage {
                key::SPACE | key::ENTER => Outcome::Action(i),
                _ => Outcome::Nothing,
            },
            Focus::Bar(b) => match usage {
                key::SPACE | key::ENTER => self.press(b),
                key::LEFT => self.step_bar(false),
                key::RIGHT => self.step_bar(true),
                _ => Outcome::Nothing,
            },
        }
    }

    fn step_bar(&mut self, on: bool) -> Outcome {
        let keys: alloc::vec::Vec<Bar> = BAR.into_iter().filter(|&b| self.enabled(b)).collect();
        let Focus::Bar(b) = self.focus else { return Outcome::Nothing };
        let at = keys.iter().position(|&k| k == b).unwrap_or(0);
        let to = if on { (at + 1).min(keys.len() - 1) } else { at.saturating_sub(1) };
        self.focus = Focus::Bar(keys[to]);
        Outcome::Changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::personalisation::Row;

    fn shell() -> Shell {
        Shell::new(pers::defaults(), time::defaults())
    }

    #[test]
    fn nothing_is_saved_until_apply_and_cancel_puts_it_back() {
        let mut s = shell();
        assert!(!s.enabled(Bar::Apply), "Apply is dark with nothing changed");
        assert_eq!(s.choose(4, 1), Outcome::Previewed);
        assert!(s.pending.clock_24h && !s.saved.clock_24h, "chosen, not saved");
        assert!(s.enabled(Bar::Apply) && s.panel_dirty(Panel::Personalisation));
        assert!(!s.panel_dirty(Panel::About));
        assert_eq!(s.press(Bar::Apply), Outcome::Apply);
        s.saved();
        assert!(s.saved.clock_24h && !s.dirty() && !s.enabled(Bar::Apply));
        s.choose(0, 1);
        assert_eq!(s.key(key::ESCAPE, false), Outcome::Cancel, "Escape is Cancel");
        s.cancel();
        assert!(s.pending.dark && s.saved.clock_24h && !s.dirty());
    }

    #[test]
    fn restore_defaults_resets_the_open_panel_only() {
        let mut s = shell();
        s.choose(1, 2);
        s.choose(5, 3);
        assert!(s.enabled(Bar::Defaults));
        assert_eq!(s.press(Bar::Defaults), Outcome::Previewed);
        assert_eq!(s.pending, pers::defaults());
        assert!(!s.enabled(Bar::Defaults));
        s.choose(1, 2);
        s.show(Panel::About);
        assert!(!s.enabled(Bar::Defaults), "About has no defaults");
        assert_eq!(s.press(Bar::Defaults), Outcome::Nothing);
        assert!(s.panel_dirty(Panel::Personalisation), "the dot stays where the change is");
    }

    #[test]
    fn the_tab_list_is_one_stop_and_its_arrows_cycle() {
        let mut s = shell();
        assert_eq!(s.key(key::DOWN, false), Outcome::Changed);
        assert_eq!(s.panel, Panel::Accounts);
        for _ in 0..5 {
            s.key(key::DOWN, false);
        }
        assert_eq!(s.panel, Panel::Personalisation, "↓ from the last wraps");
        s.key(key::UP, false);
        assert_eq!(s.panel, Panel::About, "↑ from the first wraps");
        s.key(key::HOME, false);
        assert_eq!(s.panel, Panel::Personalisation);
        s.key(key::END, false);
        assert_eq!(s.panel, Panel::About);
        // About: the tab list, then the bar (OK, Cancel), then round again.
        assert_eq!(s.stops(), [Focus::Tabs, Focus::Bar(Bar::Ok), Focus::Bar(Bar::Cancel)]);
        for _ in 0..3 {
            s.key(key::TAB, false);
        }
        assert_eq!(s.focus, Focus::Tabs);
        s.key(key::TAB, true);
        assert_eq!(s.focus, Focus::Bar(Bar::Cancel), "Shift+Tab goes back");
    }

    #[test]
    fn each_row_is_one_stop_worked_by_its_keys() {
        let mut s = shell();
        assert_eq!(s.stops().len(), 1 + ROWS.len() + 2, "every row, then OK and Cancel");
        s.key(key::TAB, false);
        assert_eq!(s.focus, Focus::Control(0));
        assert_eq!(s.key(key::RIGHT, false), Outcome::Previewed);
        assert!(!s.pending.dark, "→ chose Light");
        s.key(key::RIGHT, false);
        assert!(s.pending.dark, "a radio wraps");
        s.focus = Focus::Control(2);
        s.key(key::RIGHT, false);
        assert_eq!(s.pending.scale_pct, 100);
        s.key(key::END, false);
        assert_eq!(s.pending.scale_pct, 300);
        assert_eq!(s.key(key::RIGHT, false), Outcome::Nothing, "a stepper stops at its end");
        s.focus = Focus::Control(3);
        s.key(key::SPACE, false);
        assert!(s.pending.reduce_motion, "Space works the switch");
        // No screensaver: the idle time leaves the tab order.
        s.focus = Focus::Control(5);
        s.key(key::END, false);
        assert!(!Row::Idle.enabled(&s.pending));
        assert!(!s.stops().contains(&Focus::Control(6)));
        // Now Apply is a stop too.
        assert!(s.stops().contains(&Focus::Bar(Bar::Apply)));
    }

    #[test]
    fn the_zone_is_chosen_saved_and_put_back_apart_from_the_look() {
        let mut s = shell();
        s.show(Panel::Time);
        assert_eq!(s.stops().len(), 1 + 2 + 2, "the tab list, two rows, OK and Cancel");
        assert!(!s.enabled(Bar::Defaults), "already the defaults");
        s.key(key::TAB, false);
        assert_eq!(s.focus, Focus::Control(0));
        for _ in 0..8 {
            s.key(key::LEFT, false);
        }
        assert_eq!(s.zone_pending.offset_min, -360, "eight steps west of UTC: UTC\u{2212}06:00");
        s.key(key::TAB, false);
        assert_eq!(s.key(key::LEFT, false), Outcome::Changed);
        assert!(s.zone_pending.rtc_local, "Local time");
        assert!(s.panel_dirty(Panel::Time) && !s.panel_dirty(Panel::Personalisation));
        assert!(s.enabled(Bar::Apply) && s.zone_saved == time::defaults(), "chosen, not saved");
        s.saved();
        assert_eq!(s.zone_saved, Zone { offset_min: -360, rtc_local: true });
        assert!(!s.dirty());
        // Changed again, then Escape: back to what's saved.
        s.choose(0, 15);
        s.cancel();
        assert_eq!(s.zone_pending.offset_min, -360);
        // Restore Defaults resets only this panel.
        s.choose(1, 1);
        s.show(Panel::Personalisation);
        s.choose(4, 1);
        s.show(Panel::Time);
        assert_eq!(s.press(Bar::Defaults), Outcome::Changed);
        assert_eq!(s.zone_pending, time::defaults());
        assert!(s.pending.clock_24h, "the look's change stays");
    }

    #[test]
    fn the_accounts_keys_are_actions() {
        let mut s = shell();
        s.actions = 3;
        s.show(Panel::Accounts);
        s.key(key::TAB, false);
        s.key(key::TAB, false);
        assert_eq!(s.key(key::ENTER, false), Outcome::Action(1));
    }
}
