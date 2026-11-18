// SPDX-License-Identifier: Apache-2.0
//! Input routing (TASK-057, FR-028): keys and the mouse, as drv-i8042 sends
//! them, become what each window should hear, what moves, and what must be
//! redrawn. Pure state, host-tested; the service carries out the effects.
//!
//! - Keys pass the global shortcuts first; the rest go to the focused window.
//! - The pointer starts at the screen's centre and stays on it. Pressing on a
//!   window raises and focuses it (panels stay below and don't take focus);
//!   on its strip, dragging moves it; on its close button, it's asked to
//!   close. While a button is held, the window it went down on hears the
//!   pointer even outside it.

use crate::shortcuts::{Action, Outcome, Shortcuts};
use crate::windows::{Part, Rect, WindowId, Windows};
use alloc::vec::Vec;
use fwire::proto::stage::{Key, Kind, Pointer, PointerKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Key(Key),
    Pointer(Pointer),
    Focus(bool),
    CloseRequested,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Tell a window's client.
    Send(WindowId, Event),
    /// This part of the screen must be composed again.
    Damage(Rect),
    /// A shortcut for someone else: the launcher, Monitor, Terminal.
    Action(Action),
}

#[derive(Debug)]
pub struct Desk {
    pub windows: Windows,
    pub x: i32,
    pub y: i32,
    pub buttons: u8,
    /// The window being dragged by its strip, and where on the frame it was held.
    drag: Option<(WindowId, i32, i32)>,
    /// The window a held button went down on.
    grab: Option<WindowId>,
    shortcuts: Shortcuts,
    /// Alt+Tab: the order it walks and where it's got to.
    cycle: Option<(Vec<WindowId>, usize)>,
}

const LEFT: u8 = 1;

impl Desk {
    pub fn new(windows: Windows) -> Desk {
        let s = windows.screen();
        Desk {
            x: s.w as i32 / 2,
            y: s.h as i32 / 2,
            windows,
            buttons: 0,
            drag: None,
            grab: None,
            shortcuts: Shortcuts::default(),
            cycle: None,
        }
    }

    /// Focuses (and raises) `id`, telling both windows.
    pub fn focus(&mut self, id: WindowId, out: &mut Vec<Effect>) {
        let old = self.windows.focused();
        let area = self.windows.focus(id);
        if !area.is_empty() {
            out.push(Effect::Damage(area));
        }
        if old != Some(id) {
            if let Some(o) = old {
                out.push(Effect::Send(o, Event::Focus(false)));
            }
            out.push(Effect::Send(id, Event::Focus(true)));
        }
    }

    /// A window made by a client.
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &mut self,
        title: &str,
        kind: Kind,
        size: (u32, u32),
        min: (u32, u32),
        at: (i32, i32),
        owner: (u32, &str),
        out: &mut Vec<Effect>,
    ) -> WindowId {
        let old = self.windows.focused();
        let id = self.windows.create(title, kind, size, min, at, owner);
        if let Some(w) = self.windows.get(id) {
            out.push(Effect::Damage(w.frame));
        }
        if self.windows.focused() == Some(id) {
            if let Some(o) = old.and_then(|o| self.windows.get(o)) {
                out.push(Effect::Damage(o.frame));
                out.push(Effect::Send(o.id, Event::Focus(false)));
            }
            out.push(Effect::Send(id, Event::Focus(true)));
        }
        id
    }

    /// A window gone (closed by its client, or its client gone).
    pub fn close(&mut self, id: WindowId, out: &mut Vec<Effect>) {
        let before = self.windows.focused();
        if let Some(area) = self.windows.close(id) {
            out.push(Effect::Damage(area));
        }
        if self.drag.is_some_and(|d| d.0 == id) {
            self.drag = None;
        }
        if self.grab == Some(id) {
            self.grab = None;
        }
        if let Some((order, _)) = &mut self.cycle {
            order.retain(|&w| w != id);
        }
        let after = self.windows.focused();
        if before == Some(id)
            && let Some(n) = after
        {
            out.push(Effect::Send(n, Event::Focus(true)));
        }
    }

    pub fn key(&mut self, k: Key, out: &mut Vec<Effect>) {
        match self.shortcuts.key(k.usage, k.down, k.modifiers) {
            Outcome::Pass => {
                if let Some(f) = self.windows.focused() {
                    out.push(Effect::Send(f, Event::Key(k)));
                }
            }
            Outcome::Swallow => {}
            Outcome::Fire(Action::Cycle { back }) => {
                let (order, at) = self.cycle.get_or_insert_with(|| (self.windows.cycle_order(), 0));
                if order.is_empty() {
                    return;
                }
                let n = order.len();
                *at = if back { (*at + n - 1) % n } else { (*at + 1) % n };
                let id = order[*at];
                self.focus(id, out);
            }
            Outcome::Fire(Action::CycleDone) => self.cycle = None,
            Outcome::Fire(Action::CloseFocused) => {
                if let Some(f) = self.windows.focused() {
                    out.push(Effect::Send(f, Event::CloseRequested));
                }
            }
            Outcome::Fire(a) => out.push(Effect::Action(a)),
        }
    }

    /// The mouse moved by (dx, dy) (right and down positive), the wheel by
    /// `wheel` (up positive), with `buttons` now held.
    pub fn mouse(&mut self, dx: i32, dy: i32, wheel: i32, buttons: u8, out: &mut Vec<Effect>) {
        let s = self.windows.screen();
        self.x = (self.x + dx).clamp(0, s.w as i32 - 1);
        self.y = (self.y + dy).clamp(0, s.h as i32 - 1);
        let pressed = buttons & !self.buttons;
        let released = self.buttons & !buttons;
        self.buttons = buttons;

        if let Some((id, ox, oy)) = self.drag {
            if dx != 0 || dy != 0 {
                let area = self.windows.move_to(id, self.x - ox, self.y - oy);
                out.push(Effect::Damage(area));
            }
            if released & LEFT != 0 {
                self.drag = None;
            }
            return;
        }

        if pressed & LEFT != 0 && self.grab.is_none() {
            match self.windows.hit(self.x, self.y) {
                Some((id, Part::Close)) => {
                    self.focus(id, out);
                    out.push(Effect::Send(id, Event::CloseRequested));
                    return;
                }
                Some((id, Part::Strip)) => {
                    self.focus(id, out);
                    if let Some(w) = self.windows.get(id) {
                        self.drag = Some((id, self.x - w.frame.x, self.y - w.frame.y));
                    }
                    return;
                }
                Some((id, Part::Surface { .. })) => {
                    if self.windows.get(id).is_some_and(|w| w.kind != Kind::Panel) {
                        self.focus(id, out);
                    }
                    self.grab = Some(id);
                }
                None => {}
            }
        }

        let kind = if pressed != 0 {
            PointerKind::Down
        } else if released != 0 {
            PointerKind::Up
        } else if wheel != 0 {
            PointerKind::Wheel
        } else if dx != 0 || dy != 0 {
            PointerKind::Move
        } else {
            return;
        };
        let target = self.grab.or_else(|| match self.windows.hit(self.x, self.y) {
            Some((id, Part::Surface { .. })) => Some(id),
            _ => None,
        });
        if let Some(id) = target
            && let Some(w) = self.windows.get(id)
        {
            let r = self.windows.surface_rect(w);
            let (ex, ey) = if kind == PointerKind::Wheel { (0, wheel) } else { (dx, dy) };
            out.push(Effect::Send(
                id,
                Event::Pointer(Pointer { x: self.x - r.x, y: self.y - r.y, buttons, kind, dx: ex, dy: ey }),
            ));
        }
        if buttons == 0 {
            self.grab = None;
        }
    }
}

#[cfg(test)]
mod tests;
