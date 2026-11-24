// SPDX-License-Identifier: Apache-2.0
//! Files, the program (TASK-105, FR-049): a window with the places (Home,
//! Bin) down the side, the tools across the top, and the folder's contents
//! as a list or a grid. Ctrl+N opens another window; a row dragged onto a
//! folder (in this window or another, through stage's carry and drop)
//! moves it there (with Ctrl held, copies it). Double-click or Enter opens a
//! folder. Ctrl+C, Ctrl+X and Ctrl+V copy and move through the copy engine
//! (TASK-105A), its window showing each copy; Ctrl+Z puts the last move
//! back.
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

mod copies;

use alloc::string::String;
use alloc::vec::Vec;
use app_files::browser::{Browser, Entry, Focus, Outcome, PLACES, Place, Tool, View};
use app_files::disk::{Vfs, join, split};
use app_files::key;
use app_session::import::size;
use fapp::{Look, Window, inside};
use fcore_abi::signal;
use fstd::time::{Duration, Instant};
use fui::draw::{self, fill};
use fui::layout::Rect;
use fui::theme::{TextStyle, Tok};
use fwire::proto::ax;
use fwire::proto::stage as st;

fstd::program!(run);

const SIZE: (u32, u32) = (900, 560);
const PLACES_W: u32 = 180;
const TOOLS_H: u32 = 52;
const ROW_H: u32 = 32;
const TILE: (u32, u32) = (112, 92);
/// A press that moves this far (scaled) becomes a drag.
const DRAG_PX: i32 = 6;
const DOUBLE_NS: u64 = 500_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    Place(Place),
    Tool(Tool),
    Row(usize),
}

struct Pane {
    win: Window,
    b: Browser,
    hits: Vec<(Rect, Hit)>,
    hovered: Option<Hit>,
    /// Where a press began, and on what.
    pressed: Option<(Hit, i32, i32)>,
    /// What this window carries (a path), once a press became a drag.
    carrying: Option<String>,
    /// The last click on a row: which, and when (for a double-click).
    last_click: Option<(usize, u64)>,
    /// The first row shown (the list scrolls).
    top: usize,
    /// The modifiers its last key report held (Ctrl on a drop: a copy).
    mods: u8,
}

/// What a window asks of the program.
enum Ask {
    Close,
    NewWindow,
    /// Ctrl+C (false) or Ctrl+X (true) of these.
    Clip(Vec<String>, bool),
    /// Ctrl+V into this folder (Ctrl+Shift+V: each copy read back).
    Paste(String, bool),
    /// A copy (or, `true`, a move) through the engine: sources, into.
    Copy(Vec<String>, String, bool),
    /// A move done here, by rename (from, to): Ctrl+Z puts it back.
    Moved(String, String),
    Undo,
}

fn node(role: u8, name: &str, state: u32, r: Rect, id: &str) -> ax::Node {
    let mut n = fstd::ax::node(ax::ROOT, role, name, state, (r.x, r.y, r.w, r.h));
    n.testid = String::from(id);
    n
}

/// "12:05, 19 Nov 2026" from a time in ns (UTC as the RTC keeps it).
fn when(ns: u64) -> String {
    if ns == 0 {
        return String::new();
    }
    // Kept in UTC; shown in the zone's time (TASK-180A).
    let secs = fstd::time::to_local(ns) / 1_000_000_000;
    let (y, m, d) = app_session::import::date(secs);
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let day = secs % 86_400;
    alloc::format!("{:02}:{:02}, {d} {} {y}", day / 3600, day / 60 % 60, MONTHS[(m as usize + 11) % 12])
}

impl Pane {
    fn open(home: &str) -> Result<Pane, String> {
        let win = Window::open("Files", st::Kind::Normal, SIZE)?;
        let mut b = Browser::new(home);
        b.refresh(&mut Vfs);
        Ok(Pane {
            win,
            b,
            hits: Vec::new(),
            hovered: None,
            pressed: None,
            carrying: None,
            last_click: None,
            top: 0,
            mods: 0,
        })
    }

    fn rows_shown(&self) -> usize {
        let t = self.win.theme;
        let list_h = self.win.h.saturating_sub(t.px(TOOLS_H) + t.px(64) + t.px(24));
        (list_h / t.px(ROW_H)).max(1) as usize
    }

    /// The chosen row kept in view.
    fn follow(&mut self) {
        let Some(c) = self.b.chosen else { return };
        let n = self.rows_shown();
        if c < self.top {
            self.top = c;
        } else if c >= self.top + n {
            self.top = c + 1 - n;
        }
    }

    fn draw(&mut self) {
        self.follow();
        let t = self.win.theme;
        let (w, h) = (self.win.w, self.win.h);
        self.hits.clear();
        self.win.ground(false);
        let pw = t.px(PLACES_W);
        fill(&mut self.win.surface(), Rect::new(0, 0, pw, h), t.colour(Tok::Surface).xrgb());
        // The places.
        let pad = t.px(12) as i32;
        let row_h = t.px(40);
        let mut places = Vec::new();
        for (i, p) in PLACES.into_iter().enumerate() {
            let r = Rect::new(pad, pad + i as i32 * row_h as i32, pw - 2 * pad as u32, row_h);
            let chosen = p == self.b.place;
            let hovered = self.hovered == Some(Hit::Place(p));
            {
                let mut s = self.win.surface();
                if chosen || hovered {
                    draw::round_rect(&mut s, r, 6.0 * t.scale, t.colour(Tok::Surface2));
                }
                if chosen {
                    let bar = Rect::new(r.x, r.y + t.px(10) as i32, t.px(3).max(1), row_h - t.px(20));
                    fill(&mut s, bar, t.colour(Tok::PrimaryInk).xrgb());
                }
            }
            let (_, line, _) = t.text(TextStyle::Body);
            let ink = if chosen || hovered { Tok::Text } else { Tok::Text2 };
            self.win.say(
                (r.x + t.px(14) as i32, r.y + (row_h as i32 - line as i32) / 2),
                TextStyle::Body,
                p.name(),
                ink,
            );
            let focused = chosen && self.b.focus == Focus::Places;
            let state = if chosen { ax::SELECTED } else { 0 } | if focused { ax::FOCUSED } else { 0 };
            places.push(node(ax::OPTION, p.name(), state, r, p.id()));
            self.hits.push((r, Hit::Place(p)));
        }
        let pl = Rect::new(pad, pad, pw - 2 * pad as u32, 2 * row_h);
        self.win.nodes.push(node(ax::LISTBOX, "Places", 0, pl, "files-places"));
        self.win.nodes.extend(places);
        // The tools.
        let x0 = (pw + t.px(16)) as i32;
        let key_h = t.px(34);
        let mut kx = x0;
        let ky = (t.px(TOOLS_H) as i32 - key_h as i32) / 2;
        for tool in self.b.tools() {
            let label = tool.label(self.b.view);
            let (lw, lh) = self.win.measure(TextStyle::Body, label);
            let r = Rect::new(kx, ky, lw + t.px(28), key_h);
            if self.b.enabled(tool) {
                let look = Look {
                    focused: self.b.focus == Focus::Tool(tool),
                    hovered: self.hovered == Some(Hit::Tool(tool)),
                    pressed: matches!(self.pressed, Some((Hit::Tool(x), ..)) if x == tool)
                        && self.hovered == Some(Hit::Tool(tool)),
                };
                self.win.key(r, label, false, look, tool.id());
                self.hits.push((r, Hit::Tool(tool)));
            } else {
                draw::round_rect(&mut self.win.surface(), r, 6.0 * t.scale, t.colour(Tok::Surface2));
                let at = (r.x + (r.w as i32 - lw as i32) / 2, r.y + (r.h as i32 - lh as i32) / 2);
                self.win.say(at, TextStyle::Body, label, Tok::Text3);
                self.win.nodes.push(node(ax::BUTTON, label, ax::DISABLED, r, tool.id()));
            }
            kx += (r.w + t.px(8)) as i32;
        }
        // Where we are.
        let shown_path = match self.b.place {
            Place::Bin => String::from("Bin"),
            Place::Home => {
                let rest = self.b.dir.strip_prefix(&self.b.home).unwrap_or("");
                let mut p = String::from("Home");
                for part in rest.split('/').filter(|s| !s.is_empty()) {
                    p.push_str(" ▸ ");
                    p.push_str(part);
                }
                p
            }
        };
        let py = t.px(TOOLS_H) as i32;
        self.win.say((x0, py), TextStyle::Subtitle, &shown_path, Tok::Text);
        let (pw2, ph) = self.win.measure(TextStyle::Subtitle, &shown_path);
        let mut pn = node(ax::TEXT, &shown_path, 0, Rect::new(x0, py, pw2, ph), "files-path");
        pn.value = self.b.dir.clone();
        self.win.nodes.push(pn);
        // The contents.
        let ly = py + t.px(36) as i32;
        let lw = w - pw - t.px(32);
        let lh = h.saturating_sub(ly as u32 + t.px(40));
        let list = Rect::new(x0, ly, lw, lh);
        match self.b.view {
            View::List => self.draw_list(list),
            View::Grid => self.draw_grid(list),
        }
        if self.b.entries.is_empty() {
            let words = match self.b.place {
                Place::Bin => "The bin is empty.",
                Place::Home => "This folder is empty.",
            };
            self.win.say((x0 + t.px(8) as i32, ly + t.px(40) as i32), TextStyle::Body, words, Tok::Text2);
        }
        // What went wrong, at the foot.
        if let Some(e) = self.b.error.clone() {
            let ey = (h - t.px(30)) as i32;
            self.win.say((x0, ey), TextStyle::Small, &e, Tok::Error);
            let (ew, eh) = self.win.measure(TextStyle::Small, &e);
            let mut n = node(ax::TEXT, &e, 0, Rect::new(x0, ey, ew, eh), "files-error");
            n.value = e;
            self.win.nodes.push(n);
        }
        self.win.present();
    }

    fn row_words(e: &Entry) -> (String, String) {
        match &e.binned {
            Some((_, from)) => (alloc::format!("from {from}"), String::new()),
            None if e.dir => (String::from("Folder"), when(e.modified_ns)),
            None => (size(e.size), when(e.modified_ns)),
        }
    }

    /// A row's or a tile's name, or the field naming it.
    fn name_at(&mut self, i: usize, at: (i32, i32), w: u32) {
        let t = self.win.theme;
        let e = self.b.entries[i].clone();
        if let (Some(typed), true) = (self.b.naming.clone(), self.b.chosen == Some(i)) {
            let well = Rect::new(at.0 - t.px(4) as i32, at.1 - t.px(4) as i32, w, t.px(26));
            {
                let mut s = self.win.surface();
                draw::round_rect(&mut s, well, 4.0 * t.scale, t.colour(Tok::Focus));
                let inner = Rect::new(well.x + 2, well.y + 2, well.w - 4, well.h - 4);
                draw::round_rect(&mut s, inner, 3.0 * t.scale, t.colour(Tok::Bg));
            }
            self.win.say(at, TextStyle::Body, &typed, Tok::Text);
            let cx = at.0 + if typed.is_empty() { 0 } else { self.win.measure(TextStyle::Body, &typed).0 as i32 + 1 };
            let (_, line, _) = t.text(TextStyle::Body);
            let caret = Rect::new(cx, at.1 + 2, t.px(2).max(1), line.saturating_sub(4));
            fill(&mut self.win.surface(), caret, t.colour(Tok::PrimaryInk).xrgb());
            let mut n = node(ax::TEXTBOX, "Name", ax::FOCUSED, well, "files-name");
            n.value = typed;
            self.win.nodes.push(n);
        } else {
            let shown = self.win.fonts.fit(t, TextStyle::Body, &e.name, w);
            self.win.say(at, TextStyle::Body, &shown, Tok::Text);
        }
    }

    fn glyph(&mut self, r: Rect, dir: bool) {
        let t = self.win.theme;
        let mut s = self.win.surface();
        if dir {
            let tab = Rect::new(r.x, r.y, r.w / 2, r.h / 4 + 1);
            draw::round_rect(&mut s, tab, 2.0 * t.scale, t.colour(Tok::PrimaryInk));
            let body = Rect::new(r.x, r.y + (r.h / 5) as i32, r.w, r.h - r.h / 5);
            draw::round_rect(&mut s, body, 3.0 * t.scale, t.colour(Tok::PrimaryInk));
        } else {
            draw::round_rect(&mut s, r, 3.0 * t.scale, t.colour(Tok::Text3));
            let inner = Rect::new(r.x + 2, r.y + 2, r.w.saturating_sub(4), r.h.saturating_sub(4));
            draw::round_rect(&mut s, inner, 2.0 * t.scale, t.colour(Tok::Surface));
        }
    }

    fn draw_list(&mut self, list: Rect) {
        let t = self.win.theme;
        let row = t.px(ROW_H);
        let (name_w, size_x, when_x) = (list.w * 5 / 10, list.w * 5 / 10 + t.px(8), list.w * 3 / 4);
        // The header.
        for (label, x) in [("Name", t.px(36)), ("Size", size_x), ("Modified", when_x)] {
            self.win.say((list.x + x as i32, list.y), TextStyle::Small, label, Tok::Text2);
        }
        let y0 = list.y + t.px(22) as i32;
        let n = self.rows_shown();
        let mut rows = Vec::new();
        for i in self.top..(self.top + n).min(self.b.entries.len()) {
            let e = self.b.entries[i].clone();
            let r = Rect::new(list.x, y0 + (i - self.top) as i32 * row as i32, list.w, row);
            let chosen = self.b.chosen == Some(i);
            let hovered = self.hovered == Some(Hit::Row(i));
            if chosen || hovered {
                let mut s = self.win.surface();
                draw::round_rect(&mut s, r, 6.0 * t.scale, t.colour(Tok::Surface2));
                if chosen {
                    let bar = Rect::new(r.x, r.y + t.px(6) as i32, t.px(3).max(1), row - t.px(12));
                    fill(&mut s, bar, t.colour(Tok::PrimaryInk).xrgb());
                }
            }
            let g = t.px(18);
            self.glyph(Rect::new(r.x + t.px(10) as i32, r.y + (row as i32 - g as i32) / 2, g, g), e.dir);
            let (_, line, _) = t.text(TextStyle::Body);
            let ty = r.y + (row as i32 - line as i32) / 2;
            self.name_at(i, (r.x + t.px(36) as i32, ty), name_w - t.px(40));
            let (what, at) = Self::row_words(&e);
            self.win.say((r.x + size_x as i32, ty), TextStyle::Body, &what, Tok::Text2);
            self.win.say((r.x + when_x as i32, ty), TextStyle::Body, &at, Tok::Text2);
            let focused = chosen && self.b.focus == Focus::List && self.b.naming.is_none();
            let state = if chosen { ax::SELECTED } else { 0 } | if focused { ax::FOCUSED } else { 0 };
            let mut o = node(ax::OPTION, &e.name, state, r, &alloc::format!("files-row-{}", e.name));
            o.value = if e.dir { String::from("folder") } else { what };
            rows.push(o);
            self.hits.push((r, Hit::Row(i)));
        }
        self.win.nodes.push(node(ax::LISTBOX, "Contents", 0, list, "files-list"));
        self.win.nodes.extend(rows);
    }

    fn draw_grid(&mut self, list: Rect) {
        let t = self.win.theme;
        let (tw, th) = (t.px(TILE.0), t.px(TILE.1));
        let per = (list.w / tw).max(1) as usize;
        let mut tiles = Vec::new();
        for (i, e) in self.b.entries.clone().into_iter().enumerate() {
            let (col, row) = ((i % per) as i32, (i / per) as i32);
            let r = Rect::new(list.x + col * tw as i32, list.y + row * th as i32, tw - t.px(8), th - t.px(8));
            if r.y as u32 + r.h > list.y as u32 + list.h {
                break;
            }
            let chosen = self.b.chosen == Some(i);
            if chosen || self.hovered == Some(Hit::Row(i)) {
                draw::round_rect(&mut self.win.surface(), r, 8.0 * t.scale, t.colour(Tok::Surface2));
            }
            let g = t.px(36);
            self.glyph(Rect::new(r.x + (r.w as i32 - g as i32) / 2, r.y + t.px(10) as i32, g, g), e.dir);
            self.name_at(i, (r.x + t.px(8) as i32, r.y + t.px(56) as i32), r.w - t.px(16));
            let focused = chosen && self.b.focus == Focus::List && self.b.naming.is_none();
            let state = if chosen { ax::SELECTED } else { 0 } | if focused { ax::FOCUSED } else { 0 };
            let mut o = node(ax::OPTION, &e.name, state, r, &alloc::format!("files-row-{}", e.name));
            o.value = String::from(if e.dir { "folder" } else { "file" });
            tiles.push(o);
            self.hits.push((r, Hit::Row(i)));
        }
        self.win.nodes.push(node(ax::LISTBOX, "Contents", 0, list, "files-list"));
        self.win.nodes.extend(tiles);
    }

    fn hit_at(&self, x: i32, y: i32) -> Option<Hit> {
        self.hits.iter().rev().find(|(r, _)| inside(*r, x, y)).map(|(_, h)| *h)
    }

    /// A folder at (x, y), or the folder shown: where something dropped here goes.
    fn target(&self, x: i32, y: i32) -> String {
        match self.hit_at(x, y) {
            Some(Hit::Row(i)) if self.b.entries.get(i).is_some_and(|e| e.dir) => {
                join(&self.b.dir, &self.b.entries[i].name)
            }
            _ => self.b.dir.clone(),
        }
    }

    fn click(&mut self, h: Hit) {
        match h {
            Hit::Place(p) => {
                self.b.focus = Focus::Places;
                self.b.go(&mut Vfs, p);
                self.top = 0;
            }
            Hit::Tool(tool) => {
                self.b.focus = Focus::Tool(tool);
                self.b.press(&mut Vfs, tool);
                if !self.b.enabled(tool) {
                    self.b.focus = Focus::List;
                }
            }
            Hit::Row(i) => {
                self.b.focus = Focus::List;
                let now = Instant::now().as_ns();
                let double = self.last_click.is_some_and(|(j, at)| j == i && now - at < DOUBLE_NS);
                self.last_click = Some((i, now));
                if self.b.naming.is_some() && self.b.chosen != Some(i) {
                    self.b.naming = None;
                }
                self.b.chosen = Some(i);
                if double && self.b.place == Place::Home && self.b.entries[i].dir {
                    let name = self.b.entries[i].name.clone();
                    self.b.open(&mut Vfs, &name);
                    self.top = 0;
                    self.last_click = None;
                }
            }
        }
    }

    /// A drop here (or a drag let go here) of `from` onto (x, y): a move by
    /// rename, or with Ctrl held a copy.
    fn dropped(&mut self, from: &str, x: i32, y: i32, mods: u8) -> Option<Ask> {
        let to = self.target(x, y);
        if mods & key::CTRL != 0 {
            fstd::info!("files: copying {} into {}", split(from).1, split(&to).1);
            return Some(Ask::Copy(alloc::vec![String::from(from)], to, false));
        }
        if to == split(from).0 || to == from {
            return None;
        }
        self.b.move_into(&mut Vfs, from, &to);
        if self.b.error.is_some() {
            return None;
        }
        fstd::info!("files: moved {} into {}", split(from).1, split(&to).1);
        Some(Ask::Moved(String::from(from), join(&to, split(from).1)))
    }

    /// One event, and what it asks of the program.
    fn event(&mut self, ordinal: u32, p: &[u8]) -> Option<Ask> {
        match ordinal {
            st::EV_KEY => {
                let k = fwire::from_payload::<st::Key>(p).ok()?;
                self.mods = k.modifiers;
                if !k.down {
                    return None;
                }
                let (ctrl, shift) = (k.modifiers & key::CTRL != 0, k.modifiers & key::SHIFT != 0);
                if ctrl && shift && k.usage == key::V && self.b.place == Place::Home && self.b.naming.is_none() {
                    return Some(Ask::Paste(self.b.dir.clone(), true));
                }
                if ctrl && !shift && self.b.naming.is_none() {
                    let home = self.b.place == Place::Home;
                    match k.usage {
                        key::N => return Some(Ask::NewWindow),
                        key::C | key::X if home => {
                            let p = self.b.path()?;
                            return Some(Ask::Clip(alloc::vec![p], k.usage == key::X));
                        }
                        key::V if home => return Some(Ask::Paste(self.b.dir.clone(), false)),
                        key::Z => return Some(Ask::Undo),
                        _ => {}
                    }
                }
                if self.b.key(&mut Vfs, k.usage, k.ch, ctrl, shift) == Outcome::Changed {
                    if self.b.chosen.is_none() {
                        self.top = 0;
                    }
                    self.draw();
                }
            }
            st::EV_POINTER => {
                let e = fwire::from_payload::<st::Pointer>(p).ok()?;
                let was = self.hovered;
                self.hovered = if e.kind == st::PointerKind::Leave { None } else { self.hit_at(e.x, e.y) };
                match e.kind {
                    st::PointerKind::Down if e.buttons & 1 != 0 => {
                        self.pressed = self.hovered.map(|h| (h, e.x, e.y));
                    }
                    st::PointerKind::Move if e.buttons & 1 != 0 => {
                        let far = |(x, y): (i32, i32)| {
                            (e.x - x).abs().max((e.y - y).abs()) > self.win.theme.px(DRAG_PX as u32) as i32
                        };
                        if self.carrying.is_none()
                            && self.b.place == Place::Home
                            && self.b.naming.is_none()
                            && let Some((Hit::Row(i), x, y)) = self.pressed
                            && far((x, y))
                        {
                            let path = join(&self.b.dir, &self.b.entries[i].name);
                            let carry = st::Carry { window_id: self.win.id, data: path.clone() };
                            match self.win.stage.call::<_, ()>(st::CARRY, &carry, Vec::new(), fapp::deadline()) {
                                Ok(_) => {
                                    fstd::info!("files: carrying {}", split(&path).1);
                                    self.carrying = Some(path);
                                }
                                Err(err) => fstd::warn!("files: stage won't carry it: {err:?}"),
                            }
                        }
                    }
                    st::PointerKind::Up => {
                        let pressed = self.pressed.take();
                        let mut ask = None;
                        if let Some(from) = self.carrying.take() {
                            // Let go over this window: onto a folder here.
                            if inside(Rect::new(0, 0, self.win.w, self.win.h), e.x, e.y) {
                                ask = self.dropped(&from, e.x, e.y, self.mods);
                            }
                        } else if let Some((h, ..)) = pressed.filter(|(h, ..)| Some(*h) == self.hovered) {
                            self.click(h);
                        }
                        self.draw();
                        return ask;
                    }
                    st::PointerKind::Wheel => {
                        let n = self.b.entries.len().saturating_sub(self.rows_shown());
                        self.top = if e.dy > 0 { self.top.saturating_sub(3) } else { (self.top + 3).min(n) };
                        self.draw();
                        return None;
                    }
                    _ => {}
                }
                if self.hovered != was {
                    self.draw();
                }
            }
            st::EV_DROP => {
                let d = fwire::from_payload::<st::DropEvent>(p).ok()?;
                let mut ask = None;
                if self.b.place != Place::Home {
                    self.b.error = Some(String::from("Drop it in a folder, not the bin."));
                } else if d.data.starts_with(&alloc::format!("{}/", self.b.home)) {
                    ask = self.dropped(&d.data, d.x, d.y, d.modifiers);
                    fstd::info!("files: dropped {}", split(&d.data).1);
                }
                self.draw();
                return ask;
            }
            st::EV_CLOSE_REQUESTED => return Some(Ask::Close),
            st::EV_THEME => self.draw(),
            st::EV_SCALE => {
                if let Err(e) = self.win.reopen() {
                    fstd::error!("files: opening again at the new scale: {e}");
                    return Some(Ask::Close);
                }
                self.draw();
            }
            st::EV_FOCUS => {
                // Another window may have moved something here: read it again.
                self.b.refresh(&mut Vfs);
                self.draw();
            }
            _ => {}
        }
        None
    }
}

/// No one logged in (or no storage): a window that says so, until it's closed.
fn no_home() -> i64 {
    fstd::info!("files: no one is logged in: no home to show");
    let Ok(mut win) = Window::open("Files", st::Kind::Normal, (480, 160)) else { return 0 };
    let words = "Files shows your home once you've logged in.";
    loop {
        win.ground(false);
        let t = win.theme;
        let at = (t.px(24) as i32, t.px(24) as i32);
        win.say(at, TextStyle::Body, words, Tok::Text2);
        let (w, h) = win.measure(TextStyle::Body, words);
        win.nodes.push(node(ax::TEXT, words, 0, Rect::new(at.0, at.1, w, h), "files-no-home"));
        win.present();
        let _ = win.events.0.wait(signal::READABLE | signal::PEER_CLOSED, u64::MAX);
        match win.events() {
            Ok(evs) if evs.iter().any(|(o, _)| *o == st::EV_CLOSE_REQUESTED) => return 0,
            Ok(_) => {}
            Err(_) => return 0,
        }
    }
}

/// The program: its windows, the copy window, the clipboard, the last move.
struct App {
    home: String,
    panes: Vec<Pane>,
    copies: Option<copies::Copies>,
    clip: Option<(Vec<String>, bool)>,
    /// The last move's (where it went, where it was), for Ctrl+Z.
    last_move: Vec<(String, String)>,
}

impl App {
    /// Every window's contents read again (a copy or move changed them).
    fn refresh(&mut self) {
        for p in self.panes.iter_mut() {
            p.b.refresh(&mut Vfs);
            p.draw();
        }
    }

    fn start_copy(&mut self, sources: Vec<String>, dest: String, moving: bool, verify: bool) {
        match fstd::fs::copy_start(sources.clone(), &dest, moving, verify) {
            Ok(job) => {
                if self.copies.is_none() {
                    match copies::Copies::open() {
                        Ok(c) => self.copies = Some(c),
                        Err(e) => {
                            fstd::error!("files: the copy window: {e}");
                            let _ = fstd::fs::copy_cancel(job.job);
                            return;
                        }
                    }
                }
                let now = Instant::now().as_ns();
                let run = app_files::copy_dialog::Run::new(&job, moving, &sources, &dest, &self.home, now);
                fstd::info!("files: {} ({} item(s), {} bytes)", run.title, job.total_items, job.total_bytes);
                if let Some(c) = self.copies.as_mut() {
                    c.lives.push(copies::Live { run, sources, dest });
                    c.draw();
                }
            }
            Err(e) => {
                if let Some(p) = self.panes.first_mut() {
                    p.b.error = Some(e.text);
                    p.draw();
                }
            }
        }
    }

    fn ask(&mut self, a: Ask) {
        match a {
            Ask::Clip(paths, cut) => {
                fstd::info!("files: {} {}", if cut { "cut" } else { "copied" }, paths.join(", "));
                self.clip = Some((paths, cut));
            }
            Ask::Paste(dest, verify) => {
                if let Some((paths, cut)) = self.clip.clone() {
                    if cut {
                        self.clip = None;
                    }
                    self.start_copy(paths, dest, cut, verify);
                }
            }
            Ask::Copy(sources, dest, moving) => self.start_copy(sources, dest, moving, false),
            Ask::Moved(from, to) => {
                self.last_move = alloc::vec![(to, from)];
                self.refresh();
            }
            Ask::Undo => {
                let moves = core::mem::take(&mut self.last_move);
                for (now, was) in &moves {
                    match fstd::fs::rename(now, was) {
                        Ok(()) => fstd::info!("files: undid the move of {}", split(was).1),
                        Err(e) => fstd::warn!("files: couldn't undo the move of {}: {}", split(was).1, e.describe()),
                    }
                }
                self.refresh();
            }
            Ask::Close | Ask::NewWindow => {}
        }
    }

    /// A copy finished: a move's places kept for Ctrl+Z; the windows read again.
    fn finished(&mut self, done: Vec<copies::Live>) {
        for l in done {
            let ok = l.run.phase == app_files::copy_dialog::Phase::Done;
            fstd::info!("files: {} {}", l.run.title, if ok { "done" } else { "cancelled" });
            if ok && l.run.moving {
                self.last_move = l.sources.iter().map(|s| (join(&l.dest, split(s).1), s.clone())).collect();
            }
        }
        self.refresh();
        if self.copies.as_ref().is_some_and(|c| c.lives.is_empty())
            && let Some(c) = self.copies.take()
        {
            c.win.close(self.panes.first_mut().map(|q| &mut q.win));
        }
    }
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    let user = fstd::fs::whoami().unwrap_or_default();
    if user.is_empty() {
        return no_home();
    }
    let home = alloc::format!("/home/{user}");
    let first = match Pane::open(&home) {
        Ok(p) => p,
        Err(e) => {
            fstd::error!("files: {e}");
            return 1;
        }
    };
    let mut app =
        App { home: home.clone(), panes: alloc::vec![first], copies: None, clip: None, last_move: Vec::new() };
    app.panes[0].draw();
    fstd::info!("files: open ({user}'s home, {} items)", app.panes[0].b.entries.len());
    loop {
        let mut items: Vec<[u32; 2]> =
            app.panes.iter().map(|p| [p.win.events.0.raw(), signal::READABLE | signal::PEER_CLOSED]).collect();
        if let Some(c) = &app.copies {
            items.push([c.win.events.0.raw(), signal::READABLE | signal::PEER_CLOSED]);
        }
        // A copy running: it's stepped between events, so no waiting.
        let wait = if app.copies.as_ref().is_some_and(|c| c.busy()) {
            Instant::now().as_ns()
        } else {
            Instant::after(Duration::from_secs(5))
        };
        let _ = fstd::sys::wait_many(&items, wait);
        let mut close = None;
        let mut asks = Vec::new();
        for (i, pane) in app.panes.iter_mut().enumerate() {
            let Ok(events) = pane.win.events() else {
                close = Some(i);
                continue;
            };
            for (ordinal, p) in events {
                match pane.event(ordinal, &p) {
                    Some(Ask::Close) => close = Some(i),
                    Some(a) => asks.push(a),
                    None => {}
                }
            }
        }
        if let Some(c) = app.copies.as_mut()
            && let Ok(events) = c.win.events()
        {
            for (ordinal, p) in events {
                c.event(ordinal, &p);
            }
        }
        for a in asks {
            if matches!(a, Ask::NewWindow) {
                match Pane::open(&home) {
                    Ok(mut p) => {
                        p.draw();
                        fstd::info!("files: another window ({})", app.panes.len() + 1);
                        app.panes.push(p);
                    }
                    Err(e) => fstd::error!("files: another window: {e}"),
                }
            } else {
                app.ask(a);
            }
        }
        if let Some(c) = app.copies.as_mut() {
            let done = c.step();
            if !done.is_empty() {
                app.finished(done);
            }
        }
        if let Some(i) = close {
            let p = app.panes.remove(i);
            let heir = match app.panes.first_mut() {
                Some(q) => Some(&mut q.win),
                None => app.copies.as_mut().map(|c| &mut c.win),
            };
            p.win.close(heir);
            if app.panes.is_empty() {
                if let Some(c) = app.copies.take() {
                    for l in &c.lives {
                        let _ = fstd::fs::copy_cancel(l.run.job);
                    }
                    c.win.close(None);
                }
                fstd::info!("files: closed");
                return 0;
            }
        }
    }
}
