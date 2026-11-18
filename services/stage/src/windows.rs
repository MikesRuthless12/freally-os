// SPDX-License-Identifier: Apache-2.0
//! The windows (TASK-057, FR-028): where each is, how they stack, which one
//! has focus. Pure state, so every rule has a host test; `compose` draws it
//! and `input` moves it.
//!
//! - Four layers, bottom to top: a panel filling the screen (a wall), normal
//!   and fullscreen windows, any other panel (the Rack), popups. Raising
//!   moves a window to the top of its own layer.
//! - A normal window's frame is its title strip above its surface; stage
//!   places it (centred, then cascading) in the work area: the screen less
//!   the bands of panels docked along its top or bottom edge, the screen's
//!   width wide. Dragged, its strip stays in the work area. Panels and
//!   popups are placed by their clients; a fullscreen window covers the
//!   screen.
//! - Focus follows raising; closing the focused window passes focus to the
//!   one used most recently before it.

use alloc::string::String;
use alloc::vec::Vec;
use fwire::proto::stage::Kind;

pub type WindowId = u32;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn right(&self) -> i32 {
        self.x + self.w as i32
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h as i32
    }

    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }

    pub fn intersect(&self, o: &Rect) -> Rect {
        let (x0, y0) = (self.x.max(o.x), self.y.max(o.y));
        let (x1, y1) = (self.right().min(o.right()), self.bottom().min(o.bottom()));
        if x1 <= x0 || y1 <= y0 { Rect::default() } else { Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32) }
    }

    /// The smallest rectangle holding both (an empty one adds nothing).
    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let (x0, y0) = (self.x.min(o.x), self.y.min(o.y));
        let (x1, y1) = (self.right().max(o.right()), self.bottom().max(o.bottom()));
        Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32)
    }
}

/// Where a point falls on a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// The close button, at the strip's right end.
    Close,
    /// The rest of the title strip: drag to move.
    Strip,
    /// The surface, at these surface coordinates.
    Surface { x: i32, y: i32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub id: WindowId,
    pub title: String,
    pub kind: Kind,
    /// The whole window on screen: the title strip (normal windows) and the surface.
    pub frame: Rect,
    /// The surface's size.
    pub w: u32,
    pub h: u32,
    pub min_w: u32,
    pub min_h: u32,
    /// Who made it: stage's number for the connection, and the client's
    /// name as conductor gave it.
    pub client: u32,
    pub owner: String,
    /// Bottom to top: a wall, normal and fullscreen windows, other panels,
    /// popups and tooltips.
    layer: u8,
}

impl Window {
    pub fn decorated(&self) -> bool {
        self.kind == Kind::Normal
    }

    fn layer(&self) -> u8 {
        self.layer
    }
}

#[derive(Debug)]
pub struct Windows {
    /// Bottom to top.
    list: Vec<Window>,
    focus: Option<WindowId>,
    /// Most recently focused first.
    mru: Vec<WindowId>,
    next: WindowId,
    screen: (u32, u32),
    /// The title strip's height, in pixels at the current scale.
    pub strip: u32,
    /// How many normal windows have been placed: the cascade's step.
    placed: u32,
}

impl Windows {
    pub fn new(screen_w: u32, screen_h: u32, strip: u32) -> Windows {
        Windows {
            list: Vec::new(),
            focus: None,
            mru: Vec::new(),
            next: 1,
            screen: (screen_w, screen_h),
            strip,
            placed: 0,
        }
    }

    /// A new title strip height (the UI scale changed): normal windows keep
    /// their surfaces and grow or shrink their frames.
    pub fn set_strip(&mut self, strip: u32) {
        for w in self.list.iter_mut().filter(|w| w.kind == Kind::Normal) {
            w.frame.h = w.h + strip;
        }
        self.strip = strip;
    }

    pub fn screen(&self) -> Rect {
        Rect::new(0, 0, self.screen.0, self.screen.1)
    }

    /// Bottom to top.
    pub fn stack(&self) -> &[Window] {
        &self.list
    }

    pub fn get(&self, id: WindowId) -> Option<&Window> {
        self.list.iter().find(|w| w.id == id)
    }

    pub fn focused(&self) -> Option<WindowId> {
        self.focus
    }

    /// The surface's rectangle on screen.
    pub fn surface_rect(&self, w: &Window) -> Rect {
        let top = if w.decorated() { self.strip } else { 0 };
        Rect::new(w.frame.x, w.frame.y + top as i32, w.w, w.h)
    }

    pub fn strip_rect(&self, w: &Window) -> Option<Rect> {
        w.decorated().then(|| Rect::new(w.frame.x, w.frame.y, w.frame.w, self.strip))
    }

    pub fn close_rect(&self, w: &Window) -> Option<Rect> {
        self.strip_rect(w).map(|s| Rect::new(s.right() - self.strip as i32, s.y, self.strip, self.strip))
    }

    /// A new window; it takes focus unless it's a panel. Returns its id.
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &mut self,
        title: &str,
        kind: Kind,
        (w, h): (u32, u32),
        (min_w, min_h): (u32, u32),
        at: (i32, i32),
        (client, owner): (u32, &str),
    ) -> WindowId {
        let (sw, sh) = self.screen;
        let (w, h) = match kind {
            Kind::Fullscreen => (sw, sh),
            _ => (w.max(min_w).max(1).min(sw), h.max(min_h).max(1).min(sh)),
        };
        let frame_h = if kind == Kind::Normal { h + self.strip } else { h };
        let frame = match kind {
            Kind::Fullscreen => Rect::new(0, 0, sw, sh),
            Kind::Panel | Kind::Popup | Kind::Tooltip => Rect::new(at.0, at.1, w, frame_h),
            Kind::Normal => {
                // Centred in the work area, each next one 32 px further down
                // and right, wrapping; kept in the work area if it fits.
                let step = 32 * (self.placed % 8) as i32;
                self.placed += 1;
                let (top, bottom) = self.work();
                let x = (sw as i32 - w as i32) / 2 + step;
                let y = top + (bottom - top - frame_h as i32) / 2 + step;
                let y = y.min(bottom - frame_h as i32).max(top.min(sh as i32 - frame_h as i32)).max(0);
                Rect::new(x.clamp(0, (sw - w) as i32), y, w, frame_h)
            }
        };
        let id = self.next;
        self.next += 1;
        let wall = frame.x <= 0 && frame.y <= 0 && frame.w >= sw && frame.h >= sh;
        let layer = match kind {
            Kind::Panel if wall => 0,
            Kind::Normal | Kind::Fullscreen => 1,
            Kind::Panel => 2,
            Kind::Popup | Kind::Tooltip => 3,
        };
        let win = Window {
            id,
            title: String::from(title),
            kind,
            frame,
            w,
            h,
            min_w,
            min_h,
            client,
            owner: String::from(owner),
            layer,
        };
        let at = self.list.iter().position(|o| o.layer() > layer).unwrap_or(self.list.len());
        self.list.insert(at, win);
        if !matches!(kind, Kind::Panel | Kind::Tooltip) {
            self.focus(id);
        }
        id
    }

    /// Raises `id` to the top of its layer and gives it focus. Returns the
    /// area that may look different (the old focus's frame too).
    pub fn focus(&mut self, id: WindowId) -> Rect {
        let Some(i) = self.list.iter().position(|w| w.id == id) else { return Rect::default() };
        let mut changed = self.list[i].frame;
        if let Some(old) = self.focus.and_then(|f| self.get(f)) {
            changed = changed.union(&old.frame);
        }
        let win = self.list.remove(i);
        let layer = win.layer();
        let at = self.list.iter().position(|o| o.layer() > layer).unwrap_or(self.list.len());
        self.list.insert(at, win);
        self.focus = Some(id);
        self.mru.retain(|&m| m != id);
        self.mru.insert(0, id);
        changed
    }

    /// Closes `id`; focus passes to the most recent window before it.
    /// Returns the area it covered, or `None` if there's no such window.
    pub fn close(&mut self, id: WindowId) -> Option<Rect> {
        let i = self.list.iter().position(|w| w.id == id)?;
        let mut gone = self.list.remove(i).frame;
        self.mru.retain(|&m| m != id);
        if self.focus == Some(id) {
            self.focus = None;
            if let Some(&next) = self.mru.first() {
                gone = gone.union(&self.focus(next));
            }
        }
        Some(gone)
    }

    /// The work area's top and bottom: the screen less the bands of panels
    /// docked along its top or bottom edge, the screen's width wide.
    pub fn work(&self) -> (i32, i32) {
        let (sw, sh) = (self.screen.0, self.screen.1 as i32);
        let docked = |w: &&Window| w.layer == 2 && w.frame.x <= 0 && w.frame.w >= sw;
        let top =
            self.list.iter().filter(docked).filter(|w| w.frame.y <= 0).map(|w| w.frame.y + w.frame.h as i32).max();
        let bottom =
            self.list.iter().filter(docked).filter(|w| w.frame.y + w.frame.h as i32 >= sh).map(|w| w.frame.y).min();
        (top.unwrap_or(0).clamp(0, sh), bottom.unwrap_or(sh).clamp(0, sh))
    }

    /// Moves a window's frame to (x, y), kept where its strip can still be
    /// grabbed: the strip's top in the work area and 48 px of it across.
    /// Returns the area that changed (old and new frames).
    pub fn move_to(&mut self, id: WindowId, x: i32, y: i32) -> Rect {
        let sw = self.screen.0;
        let (top, bottom) = self.work();
        let grip = 48.min(sw as i32);
        let strip = self.strip as i32;
        let Some(w) = self.list.iter_mut().find(|w| w.id == id) else { return Rect::default() };
        let x = x.clamp(grip - w.frame.w as i32, sw as i32 - grip);
        let y = y.clamp(top, (bottom - strip).max(top));
        let old = w.frame;
        w.frame.x = x;
        w.frame.y = y;
        old.union(&w.frame)
    }

    pub fn set_title(&mut self, id: WindowId, title: &str) -> Option<Rect> {
        let strip = self.strip;
        let w = self.list.iter_mut().find(|w| w.id == id)?;
        w.title = String::from(title);
        Some(Rect::new(w.frame.x, w.frame.y, w.frame.w, strip))
    }

    /// The topmost window under (x, y), and where on it.
    pub fn hit(&self, x: i32, y: i32) -> Option<(WindowId, Part)> {
        // A tooltip lets the pointer through.
        let w = self.list.iter().rev().find(|w| w.kind != Kind::Tooltip && w.frame.contains(x, y))?;
        if let Some(c) = self.close_rect(w)
            && c.contains(x, y)
        {
            return Some((w.id, Part::Close));
        }
        if let Some(s) = self.strip_rect(w)
            && s.contains(x, y)
        {
            return Some((w.id, Part::Strip));
        }
        let s = self.surface_rect(w);
        Some((w.id, Part::Surface { x: x - s.x, y: y - s.y }))
    }

    /// The windows Alt+Tab walks: normal and fullscreen ones, most recently
    /// used first.
    pub fn cycle_order(&self) -> Vec<WindowId> {
        let mut out: Vec<WindowId> = self
            .mru
            .iter()
            .copied()
            .filter(|&id| self.get(id).is_some_and(|w| matches!(w.kind, Kind::Normal | Kind::Fullscreen)))
            .collect();
        // Any never focused, top first.
        for w in self.list.iter().rev() {
            if matches!(w.kind, Kind::Normal | Kind::Fullscreen) && !out.contains(&w.id) {
                out.push(w.id);
            }
        }
        out
    }

    /// Every window connection `client` made.
    pub fn owned_by(&self, client: u32) -> Vec<WindowId> {
        self.list.iter().filter(|w| w.client == client).map(|w| w.id).collect()
    }
}

#[cfg(test)]
mod tests;
