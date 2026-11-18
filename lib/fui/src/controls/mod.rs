// SPDX-License-Identifier: Apache-2.0
//! The controls (TASK-056, FR-027): what each does with a key, and how it's
//! drawn. Behaviour is plain state, so every rule has a host test; drawing
//! uses the theme's tokens (the hardware set's FIMGs take over the bodies
//! as the art for each lands, TASK-051/052).
//!
//! - The fleet's **toggles under the pointer**: off and away is gray, off
//!   under the pointer white, on is Freally green whether under the pointer
//!   or not. The on rule outranks the hover rule.
//! - The fleet's **knob**: a dark body, a lit arc in Freally green from its
//!   start (7:30) to the pointer, the rest of its track dim, and a green
//!   pointer line.
//! - **Focus**: Tab and Shift+Tab walk the focusable controls in tree order,
//!   wrapping; the focused one wears a 2 px `--color-focus` ring.

use crate::a11y::Role;
use crate::color::Rgba;
use crate::theme::{Theme, Tok};
use crate::tree::{NodeId, Tree};
use alloc::string::String;
use alloc::vec::Vec;

pub mod paint;

/// Keys by Freally key code (HID usage, as drv-i8042 reports them).
pub mod key {
    pub const A: u16 = 0x04;
    pub const ENTER: u16 = 0x28;
    pub const ESCAPE: u16 = 0x29;
    pub const BACKSPACE: u16 = 0x2A;
    pub const TAB: u16 = 0x2B;
    pub const SPACE: u16 = 0x2C;
    pub const HOME: u16 = 0x4A;
    pub const PAGE_UP: u16 = 0x4B;
    pub const DELETE: u16 = 0x4C;
    pub const END: u16 = 0x4D;
    pub const PAGE_DOWN: u16 = 0x4E;
    pub const RIGHT: u16 = 0x4F;
    pub const LEFT: u16 = 0x50;
    pub const DOWN: u16 = 0x51;
    pub const UP: u16 = 0x52;
}

/// One key press, as the input service delivers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Press {
    pub usage: u16,
    /// The character it types, if any.
    pub ch: Option<char>,
    pub shift: bool,
    pub ctrl: bool,
}

impl Press {
    pub fn key(usage: u16) -> Press {
        Press { usage, ..Press::default() }
    }

    fn activates(&self) -> bool {
        self.usage == key::ENTER || self.usage == key::SPACE
    }
}

/// What a key did to a control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Done {
    /// Not this control's key.
    No,
    /// Moved something inside (a caret, a highlight).
    Moved,
    /// Its value changed.
    Changed,
    /// It acted (a key pressed, a menu item chosen).
    Activated,
    /// It closed (a menu, a dialog: Escape is Cancel).
    Closed,
}

/// The fleet's toggles-under-the-pointer colours, as Flipcopy (the
/// reference) draws them: off is gray (`--color-text-2`), off under the
/// pointer white (`--color-text`: white on the dark ground, ink on the
/// light), on Freally green (`--color-primary-ink`) under the pointer or
/// not. ⛔ On outranks hover.
pub fn toggle_colour(theme: &Theme, on: bool, hovered: bool) -> Rgba {
    if on {
        theme.colour(Tok::PrimaryInk)
    } else if hovered {
        theme.colour(Tok::Text)
    } else {
        theme.colour(Tok::Text2)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Key {
    pub pressed: bool,
}

impl Key {
    pub fn press(&mut self, p: Press) -> Done {
        if p.activates() { Done::Activated } else { Done::No }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Switch {
    pub on: bool,
}

impl Switch {
    pub fn press(&mut self, p: Press) -> Done {
        if p.activates() {
            self.on = !self.on;
            Done::Changed
        } else {
            Done::No
        }
    }
}

/// A value from 0 to 1 moved in steps: the Fader's and the Knob's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ranged {
    pub value: f32,
    pub step: f32,
}

impl Ranged {
    pub fn new(value: f32, step: f32) -> Ranged {
        Ranged { value: value.clamp(0.0, 1.0), step }
    }

    pub fn press(&mut self, p: Press) -> Done {
        let v = match p.usage {
            key::UP | key::RIGHT => self.value + self.step,
            key::DOWN | key::LEFT => self.value - self.step,
            key::PAGE_UP => self.value + 10.0 * self.step,
            key::PAGE_DOWN => self.value - 10.0 * self.step,
            key::HOME => 0.0,
            key::END => 1.0,
            _ => return Done::No,
        }
        .clamp(0.0, 1.0);
        if v == self.value {
            return Done::Moved;
        }
        self.value = v;
        Done::Changed
    }
}

/// The knob's sweep: 7:30 at 0 to 4:30 at 1 (radians, counter-clockwise
/// from 3 o'clock).
pub const KNOB_START: f32 = 1.25 * core::f32::consts::PI;
pub const KNOB_SWEEP: f32 = 1.5 * core::f32::consts::PI;

/// Where the knob's pointer points for `value`.
pub fn knob_angle(value: f32) -> f32 {
    KNOB_START - value.clamp(0.0, 1.0) * KNOB_SWEEP
}

/// A meter's zone for a level: green below −12 dBFS, yellow to −3, red above.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    Green,
    Yellow,
    Red,
}

pub fn meter_zone(db: f32) -> Zone {
    if db >= -3.0 {
        Zone::Red
    } else if db >= -12.0 {
        Zone::Yellow
    } else {
        Zone::Green
    }
}

/// How many of `cells` LEDs a level lights, the ladder running −60..0 dBFS.
pub fn meter_lit(db: f32, cells: u32) -> u32 {
    (((db.clamp(-60.0, 0.0) + 60.0) / 60.0) * cells as f32 + 0.5) as u32
}

/// The Well: one line of text with a caret and a selection (in characters).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Well {
    pub text: String,
    pub caret: usize,
    /// The selection's other end, when there is one.
    pub anchor: Option<usize>,
}

impl Well {
    pub fn selection(&self) -> Option<(usize, usize)> {
        self.anchor.filter(|&a| a != self.caret).map(|a| (a.min(self.caret), a.max(self.caret)))
    }

    fn byte(&self, ch: usize) -> usize {
        self.text.char_indices().nth(ch).map_or(self.text.len(), |(b, _)| b)
    }

    fn chars(&self) -> usize {
        self.text.chars().count()
    }

    /// Takes out the selection, if any; true if there was one.
    fn cut(&mut self) -> bool {
        let Some((a, b)) = self.selection() else { return false };
        let (ba, bb) = (self.byte(a), self.byte(b));
        self.text.replace_range(ba..bb, "");
        self.caret = a;
        self.anchor = None;
        true
    }

    fn go(&mut self, to: usize, shift: bool) {
        if shift {
            self.anchor.get_or_insert(self.caret);
        } else {
            self.anchor = None;
        }
        self.caret = to;
    }

    pub fn press(&mut self, p: Press) -> Done {
        let n = self.chars();
        match p.usage {
            key::A if p.ctrl => {
                self.anchor = Some(0);
                self.caret = n;
                Done::Moved
            }
            key::LEFT => {
                let to = if !p.shift
                    && let Some((a, _)) = self.selection()
                {
                    a
                } else {
                    self.caret.saturating_sub(1)
                };
                self.go(to, p.shift);
                Done::Moved
            }
            key::RIGHT => {
                let to = if !p.shift
                    && let Some((_, b)) = self.selection()
                {
                    b
                } else {
                    (self.caret + 1).min(n)
                };
                self.go(to, p.shift);
                Done::Moved
            }
            key::HOME => {
                self.go(0, p.shift);
                Done::Moved
            }
            key::END => {
                self.go(n, p.shift);
                Done::Moved
            }
            key::BACKSPACE => {
                if !self.cut() && self.caret > 0 {
                    let (a, b) = (self.byte(self.caret - 1), self.byte(self.caret));
                    self.text.replace_range(a..b, "");
                    self.caret -= 1;
                }
                Done::Changed
            }
            key::DELETE => {
                if !self.cut() && self.caret < n {
                    let (a, b) = (self.byte(self.caret), self.byte(self.caret + 1));
                    self.text.replace_range(a..b, "");
                }
                Done::Changed
            }
            _ => match p.ch {
                Some(c) if !p.ctrl && !c.is_control() => {
                    self.cut();
                    let at = self.byte(self.caret);
                    self.text.insert(at, c);
                    self.caret += 1;
                    Done::Changed
                }
                _ => Done::No,
            },
        }
    }
}

/// A list's (and a table's rows') selection, moved by the keyboard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct List {
    pub len: usize,
    pub selected: Option<usize>,
    /// Rows a page moves.
    pub page: usize,
}

impl List {
    pub fn press(&mut self, p: Press) -> Done {
        if self.len == 0 {
            return Done::No;
        }
        let last = self.len - 1;
        let at = self.selected;
        let to = match p.usage {
            key::DOWN => at.map_or(0, |i| (i + 1).min(last)),
            key::UP => at.map_or(0, |i| i.saturating_sub(1)),
            key::HOME => 0,
            key::END => last,
            key::PAGE_DOWN => at.map_or(0, |i| (i + self.page.max(1)).min(last)),
            key::PAGE_UP => at.map_or(0, |i| i.saturating_sub(self.page.max(1))),
            _ => return Done::No,
        };
        self.selected = Some(to);
        if at == Some(to) { Done::Moved } else { Done::Changed }
    }
}

/// Tabs: across (Left/Right) or the vertical two-pane list (Up/Down); both
/// wrap, and Home/End go to the ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tabs {
    pub count: usize,
    pub selected: usize,
    pub vertical: bool,
}

impl Tabs {
    pub fn press(&mut self, p: Press) -> Done {
        if self.count == 0 {
            return Done::No;
        }
        let (back, next) = if self.vertical { (key::UP, key::DOWN) } else { (key::LEFT, key::RIGHT) };
        let to = match p.usage {
            u if u == next => (self.selected + 1) % self.count,
            u if u == back => (self.selected + self.count - 1) % self.count,
            key::HOME => 0,
            key::END => self.count - 1,
            _ => return Done::No,
        };
        let changed = to != self.selected;
        self.selected = to;
        if changed { Done::Changed } else { Done::Moved }
    }
}

/// A menu: Enter, Space or Down opens it; Up and Down move the highlight;
/// Enter chooses; Escape closes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Menu {
    pub items: usize,
    pub open: bool,
    pub highlighted: usize,
}

impl Menu {
    pub fn press(&mut self, p: Press) -> Done {
        if !self.open {
            if p.activates() || p.usage == key::DOWN {
                self.open = true;
                self.highlighted = 0;
                return Done::Moved;
            }
            return Done::No;
        }
        match p.usage {
            key::ESCAPE => {
                self.open = false;
                Done::Closed
            }
            key::DOWN => {
                self.highlighted = (self.highlighted + 1) % self.items.max(1);
                Done::Moved
            }
            key::UP => {
                self.highlighted = (self.highlighted + self.items.max(1) - 1) % self.items.max(1);
                Done::Moved
            }
            _ if p.activates() => {
                self.open = false;
                Done::Activated
            }
            _ => Done::No,
        }
    }
}

/// A dialog: Escape is Cancel (the fleet's settings rule), and closes it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Dialog {
    pub open: bool,
}

impl Dialog {
    pub fn press(&mut self, p: Press) -> Done {
        if self.open && p.usage == key::ESCAPE {
            self.open = false;
            Done::Closed
        } else {
            Done::No
        }
    }
}

/// A tooltip shows once the pointer has rested this long.
pub const TOOLTIP_DELAY_MS: u64 = 500;

pub fn tooltip_shown(rested_ms: u64) -> bool {
    rested_ms >= TOOLTIP_DELAY_MS
}

/// Whether a role takes keyboard focus.
pub fn focusable(role: Role) -> bool {
    matches!(
        role,
        Role::Button
            | Role::Switch
            | Role::Slider
            | Role::Knob
            | Role::TextField
            | Role::List
            | Role::Table
            | Role::TabList
            | Role::Menu
    )
}

/// Keyboard focus over a tree: Tab and Shift+Tab walk the focusable nodes in
/// tree order, wrapping.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Focus {
    pub order: Vec<NodeId>,
    pub at: Option<usize>,
}

impl Focus {
    pub fn of(tree: &Tree) -> Focus {
        let mut order = Vec::new();
        walk(tree, Tree::ROOT, &mut order);
        Focus { order, at: None }
    }

    pub fn current(&self) -> Option<NodeId> {
        self.at.map(|i| self.order[i])
    }

    /// Tab (or Shift+Tab): moves focus; anything else is not focus's key.
    pub fn press(&mut self, p: Press) -> Done {
        if p.usage != key::TAB || self.order.is_empty() {
            return Done::No;
        }
        let n = self.order.len();
        self.at = Some(match (self.at, p.shift) {
            (None, false) => 0,
            (None, true) => n - 1,
            (Some(i), false) => (i + 1) % n,
            (Some(i), true) => (i + n - 1) % n,
        });
        Done::Moved
    }
}

fn walk(tree: &Tree, id: NodeId, out: &mut Vec<NodeId>) {
    let n = tree.node(id);
    if focusable(n.a11y.role) && n.a11y.state & crate::a11y::state::DISABLED == 0 {
        out.push(id);
    }
    for &c in &n.children {
        walk(tree, c, out);
    }
}

/// sin and cos without libm: range-reduced Taylor series (error < 1e-6).
pub fn sin_cos(a: f32) -> (f32, f32) {
    use core::f32::consts::{FRAC_PI_2, PI, TAU};
    let sin = |x: f32| {
        let mut x = x % TAU;
        if x > PI {
            x -= TAU;
        } else if x < -PI {
            x += TAU;
        }
        if x > FRAC_PI_2 {
            x = PI - x;
        } else if x < -FRAC_PI_2 {
            x = -PI - x;
        }
        let x2 = x * x;
        x * (1.0 - x2 / 6.0 * (1.0 - x2 / 20.0 * (1.0 - x2 / 42.0 * (1.0 - x2 / 72.0))))
    };
    (sin(a), sin(a + FRAC_PI_2))
}
