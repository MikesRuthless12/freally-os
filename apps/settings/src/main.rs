// SPDX-License-Identifier: Apache-2.0
//! Settings, the program (TASK-104, FR-048): one window, one size; the
//! categories down the leading side, the chosen one's controls beside them,
//! Restore Defaults, OK, Cancel and Apply at the foot. A logged-in person's
//! look is saved by accounts (in their record) and put on the screen; a
//! guest's goes straight to stage, kept until the machine restarts.
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use app_settings::accounts::{Dialog, Kind as DKind, Outcome as DOut};
use app_settings::personalisation::{Kind, ROWS, Row, WALLPAPER_NAMES};
use app_settings::shell::{BAR, Bar, Focus, Outcome, PANELS, Panel, Shell};
use app_settings::{about, hardware, key};
use fapp::{Look as KeyLook, Window, inside};
use fcore_abi::signal;
use fimage::bundle::Bundle;
use fstd::ipc::{CallError, Client};
use fstd::time::{Duration, Instant};
use fui::draw::{self, fade, fill};
use fui::layout::Rect;
use fui::theme::{Mode, TextStyle, Tok};
use fwire::proto::accounts as a;
use fwire::proto::ax;
use fwire::proto::conductor as c;
use fwire::proto::devmgr as d;
use fwire::proto::stage as st;

fstd::program!(run);

/// The one dialog size (FR-048), at 100 %.
const SIZE: (u32, u32) = (1000, 640);
const TABS_W: u32 = 220;
const BAR_H: u32 = 64;
const ROW_H: u32 = 52;
/// Personalisation's labels' column, at 100 %.
const LABELS_W: u32 = 150;
const TILE_GAP: u32 = 12;

/// Something the pointer can press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    Tab(usize),
    /// A Personalisation row's option.
    Option(usize, usize),
    /// A stepper's − (false) or + (true).
    Step(usize, bool),
    Action(usize),
    Bar(Bar),
    Field(usize),
    Primary,
    Cancel,
    File(usize),
}

/// What an Accounts key does.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Act {
    Password,
    Export,
    Add,
    Import,
    Remove(String),
}

/// The wallpapers, small: for this theme, at this tile width, each if rendered.
type Thumbs = (Mode, u32, Vec<Option<Vec<u32>>>);

struct App {
    win: Window,
    shell: Shell,
    /// accounts, if this machine keeps them (none: a guest).
    accounts: Option<Client>,
    /// The session's account (none: a guest).
    me: Option<a::User>,
    users: Vec<a::User>,
    image: Option<fstd::machine::Image>,
    dialog: Option<Dialog>,
    /// What the pointer can press, drawn last.
    hits: Vec<(Rect, Hit)>,
    hovered: Option<Hit>,
    pressed: Option<Hit>,
    /// The live part of the look on the screen now: dark, wallpaper.
    shown: (bool, u8),
    /// The wallpapers, small, for the theme and scale they were made at.
    thumbs: Option<Thumbs>,
    /// A word on the bar: a save that failed.
    note: Option<String>,
    /// What the hardware report's last key did (About).
    report_note: Option<String>,
    /// Shown while accounts works (checking a password takes a moment).
    busy: Option<&'static str>,
}

fn call<Req: fwire::Wire, Rep: fwire::Wire>(c: &mut Client, ordinal: u32, req: &Req) -> Result<Rep, String> {
    c.call::<_, Rep>(ordinal, req, Vec::new(), Instant::after(Duration::from_secs(120))).map(|(r, _)| r).map_err(|e| {
        match e {
            CallError::Remote(_, text) => text,
            other => alloc::format!("{other:?}"),
        }
    })
}

/// `text` cut into lines no wider than `w` at `style`.
fn wrap(win: &mut Window, style: TextStyle, text: &str, w: u32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let tried = if line.is_empty() { String::from(word) } else { alloc::format!("{line} {word}") };
        if !line.is_empty() && win.measure(style, &tried).0 > w {
            lines.push(core::mem::replace(&mut line, String::from(word)));
        } else {
            line = tried;
        }
    }
    lines.push(line);
    lines
}

/// A rectangle's edge, `w` thick, drawn inside it.
fn outline(s: &mut fimage::blit::Surface, r: Rect, w: u32, c: u32) {
    fill(s, Rect::new(r.x, r.y, r.w, w), c);
    fill(s, Rect::new(r.x, r.y + r.h as i32 - w as i32, r.w, w), c);
    fill(s, Rect::new(r.x, r.y, w, r.h), c);
    fill(s, Rect::new(r.x + r.w as i32 - w as i32, r.y, w, r.h), c);
}

fn node(role: u8, name: &str, state: u32, r: Rect, id: &str) -> ax::Node {
    let mut n = fstd::ax::node(ax::ROOT, role, name, state, (r.x, r.y, r.w, r.h));
    n.testid = String::from(id);
    n
}

impl App {
    fn session(&self) -> bool {
        self.me.is_some()
    }

    /// The Accounts panel's keys, in order.
    fn actions(&self) -> Vec<(String, String, Act)> {
        let mut v = Vec::new();
        let Some(me) = &self.me else { return v };
        v.push((String::from("Change your password…"), String::from("settings-account-password"), Act::Password));
        v.push((String::from("Export your account…"), String::from("settings-account-export"), Act::Export));
        if me.owner {
            v.push((String::from("Add an account…"), String::from("settings-account-add"), Act::Add));
            v.push((String::from("Import an account…"), String::from("settings-account-import"), Act::Import));
            for u in self.users.iter().filter(|u| !u.owner) {
                v.push((
                    String::from("Remove…"),
                    alloc::format!("settings-remove-{}", u.username),
                    Act::Remove(u.username.clone()),
                ));
            }
        }
        v
    }

    fn read_users(&mut self) {
        if let Some(acc) = self.accounts.as_mut() {
            self.users = call::<_, a::Users>(acc, a::LIST, &()).map(|u| u.users).unwrap_or_default();
        }
        self.shell.actions = self.actions().len();
    }

    // ---- what's live ------------------------------------------------------

    /// The theme and the background shown as chosen (and put back by Cancel).
    fn preview(&mut self, l: &st::Look) {
        if l.dark != self.shown.0 {
            let theme = if l.dark { st::Theme::Dark } else { st::Theme::Light };
            if let Err(e) = call::<_, ()>(&mut self.win.stage, st::SET_THEME, &st::SetTheme { theme }) {
                fstd::warn!("settings: the theme: {e}");
            }
        }
        if l.wallpaper != self.shown.1 {
            let name = String::from(st::WALLPAPERS[l.wallpaper as usize % st::WALLPAPERS.len()]);
            if let Err(e) = call::<_, ()>(&mut self.win.stage, st::SET_WALLPAPER, &st::SetWallpaper { name }) {
                fstd::warn!("settings: the background: {e}");
            }
        }
        self.shown = (l.dark, l.wallpaper);
    }

    /// Apply: a person's look saved by accounts (which puts it on the
    /// screen); a guest's put on the screen here.
    fn save(&mut self) -> bool {
        let l = self.shell.pending.clone();
        let done = match (self.accounts.as_mut(), self.me.is_some()) {
            (Some(acc), true) => call::<_, ()>(acc, a::SET_LOOK, &l),
            _ => {
                let stage = &mut self.win.stage;
                let theme = if l.dark { st::Theme::Dark } else { st::Theme::Light };
                let wall = String::from(st::WALLPAPERS[l.wallpaper as usize % st::WALLPAPERS.len()]);
                let saver = if l.saver == st::NO_SAVER {
                    st::SetSaver { kind: 0, idle_ms: 0 }
                } else {
                    st::SetSaver { kind: l.saver, idle_ms: l.saver_idle_min as u32 * 60_000 }
                };
                call::<_, ()>(stage, st::SET_THEME, &st::SetTheme { theme })
                    .and_then(|_| call::<_, ()>(stage, st::SET_SCALE, &st::SetScale { pct: l.scale_pct }))
                    .and_then(|_| call::<_, ()>(stage, st::SET_WALLPAPER, &st::SetWallpaper { name: wall }))
                    .and_then(|_| call::<_, ()>(stage, st::SET_MOTION, &st::SetMotion { reduce: l.reduce_motion }))
                    .and_then(|_| call::<_, ()>(stage, st::SET_CLOCK, &st::SetClock { h24: l.clock_24h }))
                    .and_then(|_| call::<_, ()>(stage, st::SET_SAVER, &saver))
            }
        };
        match done {
            Ok(()) => {
                self.shell.saved();
                self.shown = (l.dark, l.wallpaper);
                self.note = None;
                fstd::info!("settings: saved ({})", if self.session() { "the account's look" } else { "a guest's" });
                true
            }
            Err(e) => {
                fstd::error!("settings: saving: {e}");
                self.note = Some(alloc::format!("It wasn't saved: {e}"));
                false
            }
        }
    }

    /// Cancel: what was shown live goes back.
    fn cancel(&mut self) {
        self.shell.cancel();
        let saved = self.shell.saved.clone();
        self.preview(&saved);
        fstd::info!("settings: cancelled");
    }

    // ---- drawing ------------------------------------------------------------

    fn look(&self, h: Hit, focused: bool) -> KeyLook {
        KeyLook {
            focused,
            hovered: self.hovered == Some(h),
            pressed: self.pressed == Some(h) && self.hovered == Some(h),
        }
    }

    /// A key that may be dark (not pressable now).
    fn bar_key(&mut self, r: Rect, b: Bar) {
        let on = self.shell.enabled(b);
        let focused = self.shell.focus == Focus::Bar(b) && self.dialog.is_none();
        if on {
            let look = self.look(Hit::Bar(b), focused);
            self.win.key(r, b.label(), b == Bar::Ok, look, b.id());
            self.hits.push((r, Hit::Bar(b)));
        } else {
            let t = self.win.theme;
            draw::round_rect(&mut self.win.surface(), r, 6.0 * t.scale, t.colour(Tok::Surface2));
            let (lw, lh) = self.win.measure(TextStyle::Body, b.label());
            let at = (r.x + (r.w as i32 - lw as i32) / 2, r.y + (r.h as i32 - lh as i32) / 2);
            self.win.say(at, TextStyle::Body, b.label(), Tok::Text3);
            self.win.nodes.push(node(ax::BUTTON, b.label(), ax::DISABLED, r, b.id()));
        }
    }

    /// An option of a radio row: the fleet's toggle looks (gray off, white
    /// off under the pointer, green on).
    fn pill(&mut self, r: Rect, label: &str, on: bool, hit: Hit, id: &str) {
        let t = self.win.theme;
        let hovered = self.hovered == Some(hit);
        let (ground, ink) = match (on, hovered) {
            (true, _) => (Tok::Primary, Tok::OnPrimary),
            (false, true) => (Tok::Surface2, Tok::Text),
            (false, false) => (Tok::Surface2, Tok::Text2),
        };
        draw::round_rect(&mut self.win.surface(), r, 6.0 * t.scale, t.colour(ground));
        let (lw, lh) = self.win.measure(TextStyle::Body, label);
        self.win.say(
            (r.x + (r.w as i32 - lw as i32) / 2, r.y + (r.h as i32 - lh as i32) / 2),
            TextStyle::Body,
            label,
            ink,
        );
        let state = if on { ax::CHECKED | ax::SELECTED } else { 0 } | if hovered { ax::HOVERED } else { 0 };
        self.win.nodes.push(node(ax::OPTION, label, state, r, id));
        self.hits.push((r, hit));
    }

    /// A background tile's size: four, side by side, in the room beside
    /// the labels, each the wallpaper's shape (16:10).
    fn tile(&self) -> (u32, u32) {
        let t = self.win.theme;
        let room = self.win.w.saturating_sub(t.px(TABS_W) + t.px(64) + t.px(LABELS_W));
        let tw = room.saturating_sub(3 * t.px(TILE_GAP)) / 4;
        (tw, tw * 10 / 16)
    }

    /// The wallpapers, small, for this theme (made once per theme and scale).
    fn thumbs(&mut self) -> Vec<Option<Vec<u32>>> {
        let t = self.win.theme;
        let (tw, th) = self.tile();
        if let Some((m, w, v)) = &self.thumbs
            && *m == t.mode
            && *w == tw
        {
            return v.clone();
        }
        let theme = if t.mode == Mode::Light { "light" } else { "dark" };
        let walls = fstd::service::data("os:wallpaper").ok().and_then(|b| Bundle::parse(b).ok());
        let v: Vec<Option<Vec<u32>>> = st::WALLPAPERS
            .iter()
            .map(|look| {
                let name = if *look == "studio" {
                    alloc::format!("{theme}/wallpaper")
                } else {
                    alloc::format!("{theme}/wallpaper-{look}")
                };
                let f = walls.as_ref()?.get(&name)?;
                fimage::fimg::parse(f).ok().map(|img| fimage::blit::cover(&img, tw, th))
            })
            .collect();
        self.thumbs = Some((t.mode, tw, v.clone()));
        v
    }

    fn draw_personalisation(&mut self, x: i32, y: i32, w: u32) {
        let t = self.win.theme;
        let col = x + t.px(LABELS_W) as i32;
        let cw = (x + w as i32 - col) as u32;
        let key_h = t.px(36);
        let mut y = y;
        let thumbs = self.thumbs();
        for (i, row) in ROWS.into_iter().enumerate() {
            let on = row.enabled(&self.shell.pending);
            let tall = row == Row::Wallpaper;
            let h = if tall { self.tile().1 + t.px(32) } else { t.px(ROW_H) };
            let (_, line, _) = t.text(TextStyle::Body);
            let ly = y + if tall { 0 } else { (key_h as i32 - line as i32) / 2 };
            self.win.say((x, ly), TextStyle::Body, row.label(), if on { Tok::Text } else { Tok::Text3 });
            let options = row.options();
            let chosen = row.chosen(&self.shell.pending);
            let focused = self.shell.focus == Focus::Control(i) && self.dialog.is_none();
            let area = match row.kind() {
                Kind::Radio if tall => {
                    let (tw, th) = self.tile();
                    let gap = t.px(TILE_GAP) as i32;
                    for (k, name) in WALLPAPER_NAMES.iter().enumerate() {
                        let r = Rect::new(col + k as i32 * (tw as i32 + gap), y, tw, th);
                        let hit = Hit::Option(i, k);
                        let (sel, hov) = (k == chosen, self.hovered == Some(hit));
                        {
                            let mut s = self.win.surface();
                            if sel || hov {
                                let g = t.px(if sel { 3 } else { 2 }).max(1) as i32;
                                let edge = if sel { Tok::PrimaryInk } else { Tok::Text };
                                draw::round_rect(
                                    &mut s,
                                    Rect::new(r.x - g, r.y - g, r.w + 2 * g as u32, r.h + 2 * g as u32),
                                    6.0 * t.scale + g as f32,
                                    t.colour(edge),
                                );
                            }
                            match thumbs.get(k).and_then(Option::as_ref) {
                                Some(px) => {
                                    for row_y in 0..th {
                                        let to = (r.y as u32 + row_y) * s.stride + r.x as u32;
                                        let from = (row_y * tw) as usize;
                                        s.px[to as usize..to as usize + tw as usize]
                                            .copy_from_slice(&px[from..from + tw as usize]);
                                    }
                                }
                                None => draw::round_rect(&mut s, r, 6.0 * t.scale, t.colour(Tok::Surface2)),
                            }
                        }
                        let (nw, _) = self.win.measure(TextStyle::Small, name);
                        let ink = if sel { Tok::PrimaryInk } else { Tok::Text2 };
                        self.win.say(
                            (r.x + (r.w as i32 - nw as i32) / 2, r.y + th as i32 + t.px(6) as i32),
                            TextStyle::Small,
                            name,
                            ink,
                        );
                        let state = if sel { ax::CHECKED | ax::SELECTED } else { 0 };
                        let mut n = node(ax::OPTION, name, state, r, &alloc::format!("{}-{k}", row.id()));
                        n.value = String::from(if thumbs.get(k).is_some_and(Option::is_some) {
                            "rendered"
                        } else {
                            "missing"
                        });
                        self.win.nodes.push(n);
                        self.hits.push((r, hit));
                    }
                    Rect::new(col, y, 4 * tw + 3 * gap as u32, th)
                }
                Kind::Radio => {
                    let n = options.len() as u32;
                    let gap = t.px(8);
                    let pw = ((cw - (n - 1) * gap) / n).min(t.px(132));
                    for (k, o) in options.iter().enumerate() {
                        let r = Rect::new(col + k as i32 * (pw + gap) as i32, y, pw, key_h);
                        self.pill(r, o, k == chosen, Hit::Option(i, k), &alloc::format!("{}-{k}", row.id()));
                    }
                    Rect::new(col, y, n * pw + (n - 1) * gap, key_h)
                }
                Kind::Stepper => {
                    let b = key_h;
                    let vw = t.px(160);
                    let down = Rect::new(col, y, b, b);
                    let up = Rect::new(col + (b + vw) as i32, y, b, b);
                    let at_start = chosen == 0;
                    let at_end = chosen + 1 == options.len();
                    for (r, plus, end) in [(down, false, at_start), (up, true, at_end)] {
                        let hit = Hit::Step(i, plus);
                        let id = alloc::format!("{}-{}", row.id(), if plus { "up" } else { "down" });
                        if on && !end {
                            let look = self.look(hit, false);
                            self.win.key(r, if plus { "+" } else { "−" }, false, look, &id);
                            self.hits.push((r, hit));
                        } else {
                            draw::round_rect(&mut self.win.surface(), r, 6.0 * t.scale, t.colour(Tok::Surface2));
                            let label = if plus { "+" } else { "−" };
                            let (lw, lh) = self.win.measure(TextStyle::Body, label);
                            self.win.say(
                                (r.x + (r.w as i32 - lw as i32) / 2, r.y + (r.h as i32 - lh as i32) / 2),
                                TextStyle::Body,
                                label,
                                Tok::Text3,
                            );
                            self.win.nodes.push(node(ax::BUTTON, label, ax::DISABLED, r, &id));
                        }
                    }
                    let shown = &options[chosen];
                    let (vw2, _) = self.win.measure(TextStyle::Body, shown);
                    let vx = col + b as i32 + (vw as i32 - vw2 as i32) / 2;
                    self.win.say((vx, ly), TextStyle::Body, shown, if on { Tok::Text } else { Tok::Text3 });
                    let mut n = node(
                        ax::SLIDER,
                        row.label(),
                        if on { 0 } else { ax::DISABLED } | if focused { ax::FOCUSED } else { 0 },
                        Rect::new(col, y, 2 * b + vw, b),
                        row.id(),
                    );
                    n.value = shown.clone();
                    self.win.nodes.push(n);
                    Rect::new(col, y, 2 * b + vw, b)
                }
                Kind::Switch => {
                    let r = Rect::new(col, y + (key_h as i32 - t.px(24) as i32) / 2, t.px(44), t.px(24));
                    let hit = Hit::Option(i, 1 - chosen);
                    let lit = chosen == 1;
                    let hovered = self.hovered == Some(hit);
                    {
                        let mut s = self.win.surface();
                        let track = if lit { Tok::Primary } else { Tok::Surface2 };
                        draw::round_rect(&mut s, r, r.h as f32 / 2.0, t.colour(track));
                        let k = r.h - t.px(6);
                        let kx = if lit { r.x + r.w as i32 - k as i32 - t.px(3) as i32 } else { r.x + t.px(3) as i32 };
                        let knob = match (lit, hovered) {
                            (true, _) => Tok::OnPrimary,
                            (false, true) => Tok::Text,
                            (false, false) => Tok::Text2,
                        };
                        draw::round_rect(
                            &mut s,
                            Rect::new(kx, r.y + t.px(3) as i32, k, k),
                            k as f32 / 2.0,
                            t.colour(knob),
                        );
                    }
                    self.win.say(
                        (r.x + r.w as i32 + t.px(12) as i32, ly),
                        TextStyle::Body,
                        &options[chosen],
                        Tok::Text2,
                    );
                    let state = if lit { ax::CHECKED } else { 0 } | if focused { ax::FOCUSED } else { 0 };
                    self.win.nodes.push(node(ax::SWITCH, row.label(), state, r, row.id()));
                    self.hits.push((r, hit));
                    r
                }
            };
            if focused {
                let g = t.px(4) as i32;
                let ring = Rect::new(area.x - g, area.y - g, area.w + 2 * g as u32, area.h + 2 * g as u32);
                let mut s = self.win.surface();
                outline(&mut s, ring, t.px(2).max(1), t.colour(Tok::Focus).xrgb());
            }
            if matches!(row.kind(), Kind::Radio) && !tall {
                let mut g = node(ax::GROUP, row.label(), if focused { ax::FOCUSED } else { 0 }, area, row.id());
                g.value = options[chosen].clone();
                self.win.nodes.push(g);
            } else if tall {
                let mut g = node(ax::GROUP, row.label(), if focused { ax::FOCUSED } else { 0 }, area, row.id());
                g.value = String::from(WALLPAPER_NAMES[chosen]);
                self.win.nodes.push(g);
            }
            y += h as i32;
        }
    }

    fn draw_accounts(&mut self, x: i32, y: i32, w: u32) {
        let t = self.win.theme;
        let mut y = y;
        if !self.session() {
            for l in wrap(
                &mut self.win,
                TextStyle::Body,
                "This is a guest session: there are no accounts on this machine. A look chosen here lasts until it restarts.",
                w,
            ) {
                self.win.say((x, y), TextStyle::Body, &l, Tok::Text2);
                y += t.px(24) as i32;
            }
            return;
        }
        let actions = self.actions();
        let key_h = t.px(36);
        let row = t.px(52);
        let users = self.users.clone();
        let me = self.me.as_ref().map(|m| m.username.clone()).unwrap_or_default();
        for u in &users {
            let shown = if u.display_name.is_empty() { u.username.as_str() } else { u.display_name.as_str() };
            self.win.say((x, y + t.px(4) as i32), TextStyle::Body, shown, Tok::Text);
            let role = alloc::format!(
                "{}{}{}",
                u.username,
                if u.owner { " · Owner" } else { " · Standard" },
                if u.username == me { " · you" } else { "" }
            );
            self.win.say((x, y + t.px(26) as i32), TextStyle::Small, &role, Tok::Text2);
            let mut n =
                node(ax::ROW, shown, 0, Rect::new(x, y, w, row), &alloc::format!("settings-account-{}", u.username));
            n.value = role;
            self.win.nodes.push(n);
            if let Some(k) = actions.iter().position(|a| a.2 == Act::Remove(u.username.clone())) {
                let r =
                    Rect::new(x + w as i32 - t.px(120) as i32, y + (row as i32 - key_h as i32) / 2, t.px(120), key_h);
                let look = self.look(Hit::Action(k), self.shell.focus == Focus::Control(k) && self.dialog.is_none());
                self.win.key(r, &actions[k].0, false, look, &actions[k].1);
                self.hits.push((r, Hit::Action(k)));
            }
            y += row as i32;
        }
        y += t.px(16) as i32;
        let mut kx = x;
        for (k, (label, id, act)) in actions.iter().enumerate() {
            if matches!(act, Act::Remove(_)) {
                continue;
            }
            let (lw, _) = self.win.measure(TextStyle::Body, label);
            // A key that won't fit starts the next line.
            if kx > x && kx + (lw + t.px(32)) as i32 > x + w as i32 {
                kx = x;
                y += (key_h + t.px(12)) as i32;
            }
            let r = Rect::new(kx, y, lw + t.px(32), key_h);
            let look = self.look(Hit::Action(k), self.shell.focus == Focus::Control(k) && self.dialog.is_none());
            self.win.key(r, label, false, look, id);
            self.hits.push((r, Hit::Action(k)));
            kx += (r.w + t.px(12)) as i32;
        }
    }

    fn draw_about(&mut self, x: i32, y: i32, w: u32) {
        let t = self.win.theme;
        let mut y = y;
        for (label, words, id) in about::lines(self.image.as_ref()) {
            self.win.say((x, y), TextStyle::Small, label, Tok::Text2);
            y += t.px(20) as i32;
            let lines = wrap(&mut self.win, TextStyle::Body, &words, w);
            let top = y;
            for l in &lines {
                self.win.say((x, y), TextStyle::Body, l, Tok::Text);
                y += t.px(22) as i32;
            }
            let mut n = node(ax::TEXT, label, 0, Rect::new(x, top, w, (y - top) as u32), id);
            n.value = words;
            self.win.nodes.push(n);
            y += t.px(14) as i32;
        }
        // The hardware report (TASK-111): Copy, Save, and how it begins.
        self.win.say((x, y), TextStyle::Small, "Hardware report", Tok::Text2);
        y += t.px(20) as i32;
        self.win.say(
            (x, y),
            TextStyle::Body,
            "What this machine is, as text you can send. Nothing is sent anywhere.",
            Tok::Text,
        );
        y += t.px(30) as i32;
        let key_h = t.px(36);
        let mut kx = x;
        for (k, (label, id)) in hardware::KEYS.into_iter().enumerate() {
            let (lw, _) = self.win.measure(TextStyle::Body, label);
            let r = Rect::new(kx, y, lw + t.px(32), key_h);
            let look = self.look(Hit::Action(k), self.shell.focus == Focus::Control(k) && self.dialog.is_none());
            self.win.key(r, label, false, look, id);
            self.hits.push((r, Hit::Action(k)));
            kx += (r.w + t.px(12)) as i32;
        }
        y += (key_h + t.px(12)) as i32;
        if let Some(note) = self.report_note.clone() {
            let (nw, nh) = self.win.measure(TextStyle::Small, &note);
            self.win.say((x, y), TextStyle::Small, &note, Tok::Text2);
            let mut n = node(ax::TEXT, &note, 0, Rect::new(x, y, nw, nh), "settings-report-note");
            n.value = note;
            self.win.nodes.push(n);
        }
    }

    /// The report from what the kernel, devmgr and conductor say now.
    fn report(&self) -> String {
        let mut f = hardware::Facts {
            image: about::lines(self.image.as_ref()).iter().take(2).map(|l| l.1.clone()).collect::<Vec<_>>().join(", "),
            ..Default::default()
        };
        #[cfg(target_arch = "x86_64")]
        {
            use core::arch::x86_64::{__cpuid, __cpuid_count};
            if __cpuid(0x8000_0000).eax >= 0x8000_0004 {
                let leaf = |l: u32| {
                    let r = __cpuid(l);
                    [r.eax, r.ebx, r.ecx, r.edx]
                };
                f.cpu_name = hardware::cpu_name(&[leaf(0x8000_0002), leaf(0x8000_0003), leaf(0x8000_0004)]);
            }
            let l1 = __cpuid(1);
            let ebx7 = if __cpuid(0).eax >= 7 { __cpuid_count(7, 0).ebx } else { 0 };
            f.cpu_features = hardware::cpu_features(l1.ecx, l1.edx, ebx7);
        }
        if let Some(cpus) = fstd::machine::cpus() {
            let mut b = [0u8; 16];
            let hz = fstd::sys::system_info(fcore_abi::topic::CPU, &mut b)
                .map_or(0, |_| u64::from_le_bytes([b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]]));
            f.clock = fstd::machine::clock_line(hz, cpus.tsc_from);
            f.cores = alloc::format!("{} · {}", cpus.layout(), if cpus.x2apic { "x2APIC" } else { "xAPIC" });
        }
        let mut m = [0u8; 16];
        if fstd::sys::system_info(fcore_abi::topic::MEMORY, &mut m).is_ok() {
            let word = |i: usize| {
                u64::from_le_bytes([m[i], m[i + 1], m[i + 2], m[i + 3], m[i + 4], m[i + 5], m[i + 6], m[i + 7]])
            };
            f.memory = Some((word(0), word(8)));
        }
        f.display = fstd::machine::gop_modes().1;
        let edid = fstd::machine::edid();
        if !edid.is_empty() {
            f.panel = Some(fedid::parse(&edid).ok().and_then(|e| e.name().map(String::from)).unwrap_or_default());
        }
        if let Ok(ch) = fstd::service::connect("devmgr") {
            let mut dm = Client::new(ch);
            if let Ok((t, _)) = dm.call::<_, d::FirmwareTimes>(d::FIRMWARE_TIMES, &(), Vec::new(), fapp::deadline()) {
                f.firmware = t.vendor;
            }
            if let Ok((l, _)) = dm.call::<_, d::Devices>(d::LIST, &(), Vec::new(), fapp::deadline()) {
                f.pci = l
                    .pci
                    .iter()
                    .map(|p| {
                        (
                            alloc::format!("{:02x}:{:02x}.{}", p.bus, p.device, p.function),
                            alloc::format!(
                                "{} ({:04x}:{:04x})",
                                fstd::machine::pci_class(p.class, p.subclass, p.prog_if),
                                p.vendor,
                                p.id
                            ),
                        )
                    })
                    .collect();
            }
            if let Ok((s, _)) = dm.call::<_, d::Summary>(d::SUMMARY, &(), Vec::new(), fapp::deadline()) {
                f.bound = s.bound.into_iter().map(|b| (b.device, b.driver)).collect();
            }
        }
        if let Ok(t) = fstd::service::ask::<_, c::BootTimeline>(c::BOOT_TIMELINE, &()) {
            f.boot = t
                .stages
                .into_iter()
                .map(|(id, ns)| (String::from(c::STAGES.get(id as usize).copied().unwrap_or("?")), ns as f64 / 1e6))
                .collect();
        }
        hardware::report(&f)
    }

    /// The report's key `i` pressed: 0 copies it, 1 saves it to the home.
    fn report_key(&mut self, i: usize) {
        let text = self.report();
        self.report_note = Some(if i == 0 {
            let c = st::Clip { window_id: self.win.id, text };
            match self.win.stage.call::<_, ()>(st::SET_CLIPBOARD, &c, Vec::new(), fapp::deadline()) {
                Ok(_) => {
                    fstd::info!("settings: the hardware report copied");
                    String::from("Copied: paste it into an email, or anywhere else.")
                }
                Err(e) => alloc::format!("It wasn't copied: {e:?}"),
            }
        } else {
            match &self.me {
                None => String::from("A guest has no home to save it in: log in, or copy it."),
                Some(u) => {
                    let folder = alloc::format!("/home/{}/{}", u.username, hardware::FOLDER);
                    let path = alloc::format!("{folder}/{}", hardware::FILE);
                    let _ = fstd::fs::create_dir(&folder);
                    match fstd::fs::write(&path, text.as_bytes()) {
                        Ok(()) => {
                            fstd::info!("settings: the hardware report saved to {path}");
                            alloc::format!("Saved to Home ▸ {} ▸ {}.", hardware::FOLDER, hardware::FILE)
                        }
                        Err(e) => alloc::format!("It wasn't saved: {}", e.describe()),
                    }
                }
            }
        });
        self.draw();
    }

    fn draw_dialog(&mut self) {
        let Some(d) = self.dialog.clone() else { return };
        let t = self.win.theme;
        let (ww, wh) = (self.win.w, self.win.h);
        fade(&mut self.win.surface(), Rect::new(0, 0, ww, wh), 0, 0.5);
        let pad = t.px(28) as i32;
        let mw = t.px(500);
        let inner_w = mw - 2 * pad as u32;
        let words = wrap(&mut self.win, TextStyle::Small, d.kind.words(), inner_w);
        let (_, title, _) = t.text(TextStyle::Title);
        let key_h = t.px(36);
        let file_row = t.px(36);
        let files = if d.kind == DKind::Import { d.import.files.len().max(1) as u32 } else { 0 };
        let preview_h = if d.import.preview.is_some() { t.px(44) } else { 0 };
        let body = match &d.done {
            Some(_) => t.px(60),
            None => {
                files * file_row
                    + if files > 0 { t.px(12) } else { 0 }
                    + d.fields.len() as u32 * self.win.field_height()
                    + preview_h
            }
        };
        let mh = 2 * pad as u32 + title + t.px(12) + words.len() as u32 * t.px(20) + t.px(16) + body + key_h;
        let m = Rect::new(((ww - mw) / 2) as i32, ((wh - mh.min(wh)) / 2) as i32, mw, mh.min(wh));
        {
            let mut s = self.win.surface();
            draw::round_rect(&mut s, m, 12.0 * t.scale, t.colour(Tok::Border));
            draw::round_rect(
                &mut s,
                Rect::new(m.x + 1, m.y + 1, m.w - 2, m.h - 2),
                12.0 * t.scale - 1.0,
                t.colour(Tok::Surface),
            );
        }
        self.win.nodes.push(node(ax::DIALOG, &d.kind.title(), 0, m, d.kind.id()));
        let x = m.x + pad;
        let mut y = m.y + pad;
        self.win.say((x, y), TextStyle::Title, &d.kind.title(), Tok::Text);
        y += (title + t.px(12)) as i32;
        for l in &words {
            self.win.say((x, y), TextStyle::Small, l, Tok::Text2);
            y += t.px(20) as i32;
        }
        y += t.px(16) as i32;
        if let Some(done) = &d.done {
            for l in wrap(&mut self.win, TextStyle::Body, done, inner_w) {
                self.win.say((x, y), TextStyle::Body, &l, Tok::Text);
                y += t.px(24) as i32;
            }
            let (dw, _) = self.win.measure(TextStyle::Body, done);
            self.win.nodes.push({
                let mut n = node(
                    ax::TEXT,
                    done,
                    0,
                    Rect::new(x, y - t.px(24) as i32, dw.min(inner_w), t.px(24)),
                    "settings-dialog-done",
                );
                n.value = done.clone();
                n
            });
            let r = Rect::new(
                m.x + m.w as i32 - pad - t.px(120) as i32,
                m.y + m.h as i32 - pad - key_h as i32,
                t.px(120),
                key_h,
            );
            let look = self.look(Hit::Primary, true);
            self.win.key(r, "Close", true, look, "settings-dialog-primary");
            self.hits.push((r, Hit::Primary));
            return;
        }
        if d.kind == DKind::Import {
            if d.import.files.is_empty() {
                self.win.say(
                    (x, y + t.px(8) as i32),
                    TextStyle::Body,
                    "There are no account files to import.",
                    Tok::Text2,
                );
            }
            let mut list = Vec::new();
            for (k, name) in d.import.files.iter().enumerate() {
                let r = Rect::new(x, y + k as i32 * file_row as i32, inner_w, file_row);
                let chosen = k == d.import.chosen;
                if chosen {
                    let mut s = self.win.surface();
                    draw::round_rect(&mut s, r, 6.0 * t.scale, t.colour(Tok::Surface2));
                    fill(
                        &mut s,
                        Rect::new(r.x, r.y + t.px(6) as i32, t.px(2).max(1), file_row - t.px(12)),
                        t.colour(Tok::PrimaryInk).xrgb(),
                    );
                }
                let shown = app_session::import::Import::shown(name);
                self.win.say((r.x + t.px(12) as i32, r.y + t.px(8) as i32), TextStyle::Body, shown, Tok::Text);
                let id = alloc::format!("settings-import-file-{shown}");
                list.push(node(ax::OPTION, shown, if chosen { ax::SELECTED } else { 0 }, r, &id));
                self.hits.push((r, Hit::File(k)));
            }
            let lr = Rect::new(x, y, inner_w, files * file_row);
            self.win.nodes.push(node(ax::LISTBOX, "Account files", 0, lr, "settings-import-files"));
            self.win.nodes.extend(list);
            y += (files * file_row + t.px(12)) as i32;
        }
        for (k, f) in d.fields.iter().enumerate() {
            let error = d.error.as_ref().filter(|e| e.0 == Some(k)).map(|e| e.1.as_str());
            let focused = d.focus == k && self.busy.is_none();
            let well = self.win.field((x, y, inner_w), &f.label, &f.value, f.secret, focused, error, f.id);
            self.hits.push((well, Hit::Field(k)));
            y += self.win.field_height() as i32;
            if k == 0
                && let Some(p) = &d.import.preview
            {
                self.win.say((x, y), TextStyle::Body, p, Tok::Text);
                let (pw, ph) = self.win.measure(TextStyle::Body, p);
                let mut n = node(ax::TEXT, p, 0, Rect::new(x, y, pw.min(inner_w), ph), "settings-import-preview");
                n.value = p.clone();
                self.win.nodes.push(n);
                y += preview_h as i32;
            }
        }
        let ky = m.y + m.h as i32 - pad - key_h as i32;
        if let Some(words) = self.busy {
            self.win.meter(Rect::new(x, ky, inner_w, key_h), words, "settings-dialog-meter");
            return;
        }
        if let Some((None, e)) = &d.error {
            let e = e.clone();
            self.win.say((x, ky - t.px(24) as i32), TextStyle::Small, &e, Tok::Error);
            let (ew, eh) = self.win.measure(TextStyle::Small, &e);
            let mut n = node(ax::TEXT, &e, 0, Rect::new(x, ky - t.px(24) as i32, ew, eh), "settings-dialog-error");
            n.value = e.clone();
            self.win.nodes.push(n);
        }
        let kw = t.px(150);
        let cancel = Rect::new(m.x + m.w as i32 - pad - kw as i32, ky, kw, key_h);
        let primary = Rect::new(cancel.x - t.px(12) as i32 - kw as i32, ky, kw, key_h);
        let n = d.fields.len();
        let look = self.look(Hit::Primary, d.focus == n);
        self.win.key(primary, d.primary(), true, look, "settings-dialog-primary");
        let look = self.look(Hit::Cancel, d.focus == n + 1);
        self.win.key(cancel, "Cancel", false, look, "settings-dialog-cancel");
        self.hits.push((primary, Hit::Primary));
        self.hits.push((cancel, Hit::Cancel));
    }

    fn draw(&mut self) {
        let t = self.win.theme;
        let (w, h) = (self.win.w, self.win.h);
        self.hits.clear();
        self.win.ground(false);
        // The tab list, on the raised surface.
        let tabs_w = t.px(TABS_W);
        let bar_h = t.px(BAR_H);
        fill(&mut self.win.surface(), Rect::new(0, 0, tabs_w, h - bar_h), t.colour(Tok::Surface).xrgb());
        let tab_h = t.px(44);
        let pad = t.px(12) as i32;
        let mut tabs = Vec::new();
        for (i, p) in PANELS.into_iter().enumerate() {
            let r = Rect::new(pad, pad + i as i32 * tab_h as i32, tabs_w - 2 * pad as u32, tab_h);
            let chosen = p == self.shell.panel;
            let hovered = self.hovered == Some(Hit::Tab(i));
            {
                let mut s = self.win.surface();
                if chosen || hovered {
                    draw::round_rect(&mut s, r, 6.0 * t.scale, t.colour(Tok::Surface2));
                }
                if chosen {
                    fill(
                        &mut s,
                        Rect::new(r.x, r.y + t.px(10) as i32, t.px(3).max(1), tab_h - t.px(20)),
                        t.colour(Tok::PrimaryInk).xrgb(),
                    );
                }
            }
            if chosen && self.shell.focus == Focus::Tabs && self.dialog.is_none() {
                let mut s = self.win.surface();
                outline(&mut s, r, t.px(2).max(1), t.colour(Tok::Focus).xrgb());
            }
            let (_, line, _) = t.text(TextStyle::Body);
            let ink = if chosen || hovered { Tok::Text } else { Tok::Text2 };
            self.win.say(
                (r.x + t.px(16) as i32, r.y + (tab_h as i32 - line as i32) / 2),
                TextStyle::Body,
                p.name(),
                ink,
            );
            let dirty = self.shell.panel_dirty(p);
            if dirty {
                let d = t.px(8);
                let dot =
                    Rect::new(r.x + r.w as i32 - t.px(16) as i32 - d as i32, r.y + (tab_h as i32 - d as i32) / 2, d, d);
                draw::round_rect(&mut self.win.surface(), dot, d as f32 / 2.0, t.colour(Tok::PrimaryInk));
            }
            let state = if chosen { ax::SELECTED } else { 0 }
                | if chosen && self.shell.focus == Focus::Tabs { ax::FOCUSED } else { 0 };
            let mut n = node(ax::TAB, p.name(), state, r, p.id());
            n.value = String::from(if dirty { "changed" } else { "" });
            tabs.push(n);
            self.hits.push((r, Hit::Tab(i)));
        }
        let list = Rect::new(pad, pad, tabs_w - 2 * pad as u32, PANELS.len() as u32 * tab_h);
        self.win.nodes.push(node(ax::TABLIST, "Categories", 0, list, "settings-tabs"));
        self.win.nodes.extend(tabs);
        // The panel.
        let x = (tabs_w + t.px(32)) as i32;
        let pw = w - tabs_w - t.px(64);
        let (_, title, _) = t.text(TextStyle::Title);
        let y = t.px(24) as i32;
        self.win.say((x, y), TextStyle::Title, self.shell.panel.name(), Tok::Text);
        let y = y + (title + t.px(20)) as i32;
        match self.shell.panel {
            Panel::Personalisation => self.draw_personalisation(x, y, pw),
            Panel::Accounts => self.draw_accounts(x, y, pw),
            Panel::About => self.draw_about(x, y, pw),
        }
        // The bar.
        let by = (h - bar_h) as i32;
        fill(&mut self.win.surface(), Rect::new(0, by, w, 1), t.colour(Tok::Border).xrgb());
        let key_h = t.px(36);
        let ky = by + (bar_h as i32 - key_h as i32) / 2;
        let edge = t.px(24) as i32;
        let (dw, _) = self.win.measure(TextStyle::Body, Bar::Defaults.label());
        self.bar_key(Rect::new(edge, ky, dw + t.px(32), key_h), Bar::Defaults);
        let kw = t.px(104);
        let gap = t.px(12) as i32;
        let mut kx = w as i32 - edge - kw as i32;
        for b in BAR.into_iter().rev().take(3) {
            self.bar_key(Rect::new(kx, ky, kw, key_h), b);
            kx -= kw as i32 + gap;
        }
        if let Some(n) = self.note.clone() {
            let nx = edge + (dw + t.px(48)) as i32;
            self.win.say((nx, ky + t.px(8) as i32), TextStyle::Small, &n, Tok::Error);
        }
        self.draw_dialog();
        // The dialog is on top: only its keys and fields can be pressed.
        if self.dialog.is_some() {
            self.hits.retain(|(_, h)| matches!(h, Hit::Field(_) | Hit::Primary | Hit::Cancel | Hit::File(_)));
        }
        self.win.present();
    }

    // ---- what's done ---------------------------------------------------------

    /// Settings closes: `true` to end the program.
    fn outcome(&mut self, o: Outcome) -> bool {
        match o {
            Outcome::Changed => self.draw(),
            Outcome::Previewed => {
                let l = self.shell.pending.clone();
                self.preview(&l);
                self.note = None;
                self.draw();
            }
            Outcome::Apply => {
                self.save();
                self.draw();
            }
            Outcome::Ok => {
                if !self.shell.dirty() || self.save() {
                    return true;
                }
                self.draw();
            }
            Outcome::Cancel => {
                self.cancel();
                return true;
            }
            Outcome::Action(i) if self.shell.panel == Panel::About => self.report_key(i),
            Outcome::Action(i) => self.open_dialog(i),
            Outcome::Nothing => {}
        }
        false
    }

    fn open_dialog(&mut self, i: usize) {
        let Some((_, _, act)) = self.actions().into_iter().nth(i) else { return };
        let kind = match act {
            Act::Password => DKind::Password,
            Act::Export => DKind::Export,
            Act::Add => DKind::Add,
            Act::Import => DKind::Import,
            Act::Remove(u) => DKind::Remove(u),
        };
        let files = if kind == DKind::Import {
            self.accounts
                .as_mut()
                .and_then(|acc| call::<_, a::Bundles>(acc, a::BUNDLES, &()).ok())
                .map(|b| b.names)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        fstd::info!("settings: {}", kind.title());
        self.dialog = Some(Dialog::new(kind, files));
        self.hovered = None;
        self.draw();
    }

    fn close_dialog(&mut self) {
        if let Some(mut d) = self.dialog.take() {
            d.forget();
        }
        self.read_users();
        let n = self.shell.actions;
        if matches!(self.shell.focus, Focus::Control(i) if i >= n) {
            self.shell.focus = Focus::Tabs;
        }
        self.draw();
    }

    /// The dialog's key pressed.
    fn submit(&mut self) {
        let Some(d) = self.dialog.as_mut() else { return };
        if d.done.is_some() {
            self.close_dialog();
            return;
        }
        if !d.check() {
            self.draw();
            return;
        }
        let d = self.dialog.clone().unwrap_or_else(|| Dialog::new(DKind::Password, Vec::new()));
        let importing = d.kind == DKind::Import && d.import.preview.is_none();
        self.busy = Some(match d.kind {
            DKind::Export => "Sealing your account and your files…",
            _ if importing => "Opening the file…",
            _ => "Checking the password…",
        });
        self.draw();
        let v = |i: usize| String::from(d.value(i));
        let Some(acc) = self.accounts.as_mut() else { return };
        let done: Result<Option<String>, String> = match &d.kind {
            DKind::Password => {
                let mut req = a::ChangePassword { old: v(0), new: v(1) };
                let r = call::<_, ()>(acc, a::CHANGE_PASSWORD, &req)
                    .map(|_| Some(String::from("Your password is changed.")));
                fstd::secret::wipe_str(&mut req.old);
                fstd::secret::wipe_str(&mut req.new);
                r
            }
            DKind::Export => {
                let mut req = a::Export { password: v(0) };
                let r = call::<_, a::Exported>(acc, a::EXPORT, &req)
                    .map(|e| Some(alloc::format!("Saved as {} ({}).", e.path, app_session::import::size(e.bytes))));
                fstd::secret::wipe_str(&mut req.password);
                r
            }
            DKind::Add => {
                let mut req = a::Create { username: v(0), display_name: v(1), password: v(2), owner_password: v(4) };
                let r = call::<_, ()>(acc, a::CREATE, &req).map(|_| Some(alloc::format!("{} can log in now.", v(0))));
                fstd::secret::wipe_str(&mut req.password);
                fstd::secret::wipe_str(&mut req.owner_password);
                r
            }
            DKind::Import if importing => {
                let name = String::from(d.import.file().unwrap_or(""));
                let mut req = a::Open { name, password: v(0) };
                let r = call::<_, a::Preview>(acc, a::PREVIEW, &req).map(|p| {
                    if let Some(dd) = self.dialog.as_mut() {
                        dd.opened(&p.username, p.files, p.bytes, p.exported_unix);
                    }
                    None
                });
                fstd::secret::wipe_str(&mut req.password);
                r
            }
            DKind::Import => {
                let name = String::from(d.import.file().unwrap_or(""));
                let mut req = a::Import { name, password: v(0), rename: v(1), owner_password: v(2) };
                let r = call::<_, a::User>(acc, a::IMPORT, &req)
                    .map(|u| Some(alloc::format!("{} is imported, and can log in now.", u.username)));
                fstd::secret::wipe_str(&mut req.password);
                fstd::secret::wipe_str(&mut req.owner_password);
                r
            }
            DKind::Remove(u) => {
                let mut req = a::Remove { username: u.clone(), confirm: v(0), owner_password: v(1) };
                let r = call::<_, ()>(acc, a::REMOVE, &req).map(|_| Some(alloc::format!("{u} is removed.")));
                fstd::secret::wipe_str(&mut req.owner_password);
                r
            }
        };
        let mut d = d;
        d.forget();
        self.busy = None;
        if let Some(dd) = self.dialog.as_mut() {
            match done {
                Ok(Some(words)) => {
                    fstd::info!("settings: {}", words);
                    dd.finished(words);
                }
                Ok(None) => {}
                Err(e) => {
                    fstd::info!("settings: {}: {e}", dd.kind.title());
                    dd.refused(&e);
                }
            }
        }
        self.read_users();
        self.draw();
    }

    fn click(&mut self, h: Hit) -> bool {
        match h {
            Hit::Tab(i) => {
                self.shell.focus = Focus::Tabs;
                let o = self.shell.show(PANELS[i]);
                return self.outcome(if o == Outcome::Nothing { Outcome::Changed } else { o });
            }
            Hit::Option(row, i) => {
                self.shell.focus = Focus::Control(row);
                let o = self.shell.choose(row, i);
                return self.outcome(if o == Outcome::Nothing { Outcome::Changed } else { o });
            }
            Hit::Step(row, up) => {
                self.shell.focus = Focus::Control(row);
                let at = ROWS[row].chosen(&self.shell.pending);
                let to = if up { at + 1 } else { at.saturating_sub(1) };
                let o = self.shell.choose(row, to);
                return self.outcome(if o == Outcome::Nothing { Outcome::Changed } else { o });
            }
            Hit::Action(i) => {
                self.shell.focus = Focus::Control(i);
                return self.outcome(Outcome::Action(i));
            }
            Hit::Bar(b) => {
                if self.shell.enabled(b) {
                    self.shell.focus = Focus::Bar(b);
                }
                let o = self.shell.press(b);
                return self.outcome(o);
            }
            Hit::Field(k) => {
                if let Some(d) = self.dialog.as_mut() {
                    d.focus = k;
                }
            }
            Hit::Primary => self.submit(),
            Hit::Cancel => self.close_dialog(),
            Hit::File(k) => {
                if let Some(d) = self.dialog.as_mut() {
                    d.choose(k);
                }
            }
        }
        self.draw();
        false
    }

    fn hit_at(&self, x: i32, y: i32) -> Option<Hit> {
        self.hits.iter().rev().find(|(r, _)| inside(*r, x, y)).map(|(_, h)| *h)
    }

    /// One event; `true` when Settings closes.
    fn event(&mut self, ordinal: u32, p: &[u8]) -> bool {
        match ordinal {
            st::EV_KEY => {
                let Ok(k) = fwire::from_payload::<st::Key>(p) else { return false };
                if !k.down || self.busy.is_some() {
                    return false;
                }
                let shift = k.modifiers & key::SHIFT != 0;
                if let Some(d) = self.dialog.as_mut() {
                    match d.key(k.usage, k.ch, shift) {
                        DOut::Changed => self.draw(),
                        DOut::Submit => self.submit(),
                        DOut::Close => self.close_dialog(),
                        DOut::Nothing => {}
                    }
                    return false;
                }
                let o = self.shell.key(k.usage, shift);
                self.outcome(o)
            }
            st::EV_POINTER => {
                let Ok(e) = fwire::from_payload::<st::Pointer>(p) else { return false };
                let was = (self.hovered, self.pressed);
                self.hovered = if e.kind == st::PointerKind::Leave { None } else { self.hit_at(e.x, e.y) };
                match e.kind {
                    st::PointerKind::Down if e.buttons & 1 != 0 => self.pressed = self.hovered,
                    st::PointerKind::Up => {
                        let pressed = self.pressed.take();
                        if let Some(h) = pressed.filter(|h| Some(*h) == self.hovered)
                            && self.busy.is_none()
                        {
                            return self.click(h);
                        }
                    }
                    _ => {}
                }
                if (self.hovered, self.pressed) != was {
                    self.draw();
                }
                false
            }
            // A click outside (another window took the focus): Cancel, as the
            // fleet's Settings do. Not while accounts is working.
            st::EV_FOCUS => {
                let Ok(f) = fwire::from_payload::<st::Focus>(p) else { return false };
                if f.focused || self.busy.is_some() {
                    return false;
                }
                if let Some(mut d) = self.dialog.take() {
                    d.forget();
                }
                fstd::info!("settings: another window took the focus");
                self.outcome(Outcome::Cancel)
            }
            // The window's ×: Cancel.
            st::EV_CLOSE_REQUESTED => {
                if let Some(mut d) = self.dialog.take() {
                    d.forget();
                }
                self.outcome(Outcome::Cancel)
            }
            st::EV_THEME => {
                self.shown.0 = self.win.theme.mode == Mode::Dark;
                self.draw();
                false
            }
            st::EV_SCALE => {
                // A window's size is fixed when it's made: made again.
                if let Err(e) = self.win.reopen() {
                    fstd::error!("settings: opening again at the new scale: {e}");
                    return true;
                }
                self.thumbs = None;
                self.draw();
                false
            }
            _ => false,
        }
    }
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    let win = match Window::open("Settings", st::Kind::Normal, SIZE) {
        Ok(w) => w,
        Err(e) => {
            fstd::error!("settings: {e}");
            return 1;
        }
    };
    // accounts, where this image keeps them; a session's account, if any.
    let mut accounts = fstd::service::connect_within("accounts", Duration::from_secs(2)).ok().map(Client::new);
    let me = accounts
        .as_mut()
        .and_then(|acc| call::<_, a::Who>(acc, a::SESSION, &()).ok())
        .map(|w| w.user)
        .filter(|u| !u.username.is_empty());
    let mut win = win;
    let saved = call::<_, st::Look>(&mut win.stage, st::LOOK, &())
        .unwrap_or_else(|_| app_settings::personalisation::defaults());
    let shown = (saved.dark, saved.wallpaper);
    let mut app = App {
        win,
        shell: Shell::new(saved),
        accounts,
        me,
        users: Vec::new(),
        image: fstd::machine::image(),
        dialog: None,
        hits: Vec::new(),
        hovered: None,
        pressed: None,
        shown,
        thumbs: None,
        note: None,
        report_note: None,
        busy: None,
    };
    app.read_users();
    app.draw();
    fstd::info!(
        "settings: open ({})",
        match &app.me {
            Some(u) => alloc::format!("{}'s", u.username),
            None => String::from("a guest's"),
        }
    );
    loop {
        let _ = app.win.events.0.wait(signal::READABLE | signal::PEER_CLOSED, Instant::after(Duration::from_secs(5)));
        let Ok(events) = app.win.events() else { return 0 };
        for (ordinal, p) in events {
            if app.event(ordinal, &p) {
                fstd::info!("settings: closed");
                return 0;
            }
        }
    }
}
