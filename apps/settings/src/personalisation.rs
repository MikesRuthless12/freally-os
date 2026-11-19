// SPDX-License-Identifier: Apache-2.0
//! Settings ▸ Personalisation (TASK-104, FR-048): the theme, the
//! background (one of the Freally wallpapers), the scale, Reduce Motion,
//! the clock, the screensaver and its idle time. Each row is one control
//! and one tab stop; its options come from here, and so does the look each
//! choice makes.

use alloc::string::String;
use alloc::vec::Vec;
use fwire::proto::stage::{Look, NO_SAVER};

/// The scales offered: 0 is *Automatic* (the screen's own, from its EDID).
pub const SCALES: [u16; 10] = [0, 100, 125, 150, 175, 200, 225, 250, 275, 300];
/// The idle times offered, in minutes.
pub const IDLE: [u16; 12] = [1, 2, 3, 5, 10, 15, 20, 30, 45, 60, 90, 120];
/// The screensavers, by `Look::saver`, and none.
pub const SAVERS: [u8; 5] = [0, 1, 2, 3, NO_SAVER];
const SAVER_NAMES: [&str; 5] = ["Bounce", "Turning block", "Swarm", "Warp", "None"];
/// The wallpapers' names as shown (`stage::WALLPAPERS` order).
pub const WALLPAPER_NAMES: [&str; 4] = ["Studio", "Circuit", "Orbit", "Connected"];

/// The defaults (the owner, 2026-11-18): dark, the screen's own scale, the
/// studio wall, motion as it comes, the 12-hour clock, Bounce after 10
/// minutes.
pub fn defaults() -> Look {
    Look {
        dark: true,
        scale_pct: 0,
        wallpaper: 0,
        reduce_motion: false,
        clock_24h: false,
        saver: 0,
        saver_idle_min: 10,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// One of a few, side by side (←/→ choose).
    Radio,
    /// One of many in order, with − and + (←/→ step; it stops at the ends).
    Stepper,
    /// On or off (Space).
    Switch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Theme,
    Wallpaper,
    Scale,
    Motion,
    Clock,
    Saver,
    Idle,
}

pub const ROWS: [Row; 7] = [Row::Theme, Row::Wallpaper, Row::Scale, Row::Motion, Row::Clock, Row::Saver, Row::Idle];

fn idle_words(m: u16) -> String {
    match m {
        1 => String::from("1 minute"),
        60 => String::from("1 hour"),
        120 => String::from("2 hours"),
        m => alloc::format!("{m} minutes"),
    }
}

/// Where `v` sits in `list`: its place, or the first one past it.
fn place<T: PartialOrd + Copy>(list: &[T], v: T) -> usize {
    list.iter().position(|&x| x >= v).unwrap_or(list.len() - 1)
}

impl Row {
    pub fn label(self) -> &'static str {
        match self {
            Row::Theme => "Theme",
            Row::Wallpaper => "Background",
            Row::Scale => "Scale",
            Row::Motion => "Reduce Motion",
            Row::Clock => "Clock",
            Row::Saver => "Screensaver",
            Row::Idle => "Start it after",
        }
    }

    /// The control's test id; an option's is this, `-`, and its number.
    pub fn id(self) -> &'static str {
        match self {
            Row::Theme => "settings-theme",
            Row::Wallpaper => "settings-wallpaper",
            Row::Scale => "settings-scale",
            Row::Motion => "settings-motion",
            Row::Clock => "settings-clock",
            Row::Saver => "settings-saver",
            Row::Idle => "settings-idle",
        }
    }

    pub fn kind(self) -> Kind {
        match self {
            Row::Scale | Row::Idle => Kind::Stepper,
            Row::Motion => Kind::Switch,
            _ => Kind::Radio,
        }
    }

    pub fn options(self) -> Vec<String> {
        let words = |l: &[&str]| l.iter().map(|s| String::from(*s)).collect();
        match self {
            Row::Theme => words(&["Dark", "Light"]),
            Row::Wallpaper => words(&WALLPAPER_NAMES),
            Row::Scale => SCALES
                .iter()
                .map(|&p| if p == 0 { String::from("Automatic") } else { alloc::format!("{p} %") })
                .collect(),
            Row::Motion => words(&["Off", "On"]),
            Row::Clock => words(&["12-hour", "24-hour"]),
            Row::Saver => words(&SAVER_NAMES),
            Row::Idle => IDLE.iter().map(|&m| idle_words(m)).collect(),
        }
    }

    pub fn chosen(self, l: &Look) -> usize {
        match self {
            Row::Theme => usize::from(!l.dark),
            Row::Wallpaper => (l.wallpaper as usize).min(WALLPAPER_NAMES.len() - 1),
            Row::Scale => place(&SCALES, l.scale_pct),
            Row::Motion => usize::from(l.reduce_motion),
            Row::Clock => usize::from(l.clock_24h),
            Row::Saver => SAVERS.iter().position(|&s| s == l.saver).unwrap_or(0),
            Row::Idle => place(&IDLE, l.saver_idle_min.max(1)),
        }
    }

    /// The look with option `i` chosen (`i` past the end: the last).
    pub fn set(self, l: &mut Look, i: usize) {
        let last = self.options().len() - 1;
        let i = i.min(last);
        match self {
            Row::Theme => l.dark = i == 0,
            Row::Wallpaper => l.wallpaper = i as u8,
            Row::Scale => l.scale_pct = SCALES[i],
            Row::Motion => l.reduce_motion = i == 1,
            Row::Clock => l.clock_24h = i == 1,
            Row::Saver => l.saver = SAVERS[i],
            Row::Idle => l.saver_idle_min = IDLE[i],
        }
    }

    /// The idle time means nothing with no screensaver.
    pub fn enabled(self, l: &Look) -> bool {
        self != Row::Idle || l.saver != NO_SAVER
    }

    /// Shown the moment it's chosen (and put back by Cancel): the theme and
    /// the background. The rest waits for Apply.
    pub fn live(self) -> bool {
        matches!(self, Row::Theme | Row::Wallpaper)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_option_of_every_row_reads_back_as_chosen() {
        for row in ROWS {
            for i in 0..row.options().len() {
                let mut l = defaults();
                row.set(&mut l, i);
                assert_eq!(row.chosen(&l), i, "{row:?} option {i}");
            }
        }
    }

    #[test]
    fn a_value_between_the_offered_ones_reads_as_the_next() {
        let mut l = defaults();
        l.saver_idle_min = 7;
        assert_eq!(Row::Idle.options()[Row::Idle.chosen(&l)], "10 minutes");
        l.saver_idle_min = 500;
        assert_eq!(Row::Idle.options()[Row::Idle.chosen(&l)], "2 hours");
        l.scale_pct = 0;
        assert_eq!(Row::Scale.options()[Row::Scale.chosen(&l)], "Automatic");
    }

    #[test]
    fn no_screensaver_turns_the_idle_time_off() {
        let mut l = defaults();
        assert!(Row::Idle.enabled(&l));
        Row::Saver.set(&mut l, 4);
        assert_eq!(l.saver, NO_SAVER);
        assert!(!Row::Idle.enabled(&l));
    }
}
