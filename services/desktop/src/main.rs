// SPDX-License-Identifier: Apache-2.0
//! desktop, the service (TASK-059, FR-030): the Rack along the screen's
//! foot, its tooltips, and the launcher, each a window of stage's.
//!
//! - The Rack: a panel 56 px tall. The keys are the hardware set's, and their
//!   legends are drawn until the OS's icons are rendered (TASK-052). Each LED
//!   is lit while its app runs; a key whose app the image doesn't carry is at
//!   40 %, its tooltip "Not available yet.". The 12-hour clock sits in its
//!   display well. Hover a key and its tooltip shows after 600 ms.
//! - `Super` (stage passes the shortcut on) or the launcher key opens the
//!   launcher: type to filter, ↑ ↓, Enter, Esc. `Super+T` / `Super+M` start
//!   Terminal / Monitor, once the image carries them.
//! - The wallpaper is the Blender render (TASK-052); until it's in the
//!   image, stage's ground shows instead, and no window is spent on it.
//! - S4: stamped with conductor once the Rack is on screen taking input.
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::{FErr, perm, signal, well_known};
use fimage::blit::Surface;
use fimage::bundle::Bundle;
use fstd::handle::Handle;
use fstd::ipc::{Channel, Client};
use fstd::time::{Duration, Instant};
use fui::color::Rgba;
use fui::controls::paint::{paint_key, paint_lamp};
use fui::draw;
use fui::draw::{fade, fill};
use fui::hardware::{Hardware, KeyLook, Lamp};
use fui::layout::Rect;
use fui::text::{FontData, Fonts, words};
use fui::theme::{Mode, TextStyle, Theme, Tok};
use fwire::proto::conductor as c;
use fwire::proto::stage as st;
use svc_desktop::clock;
use svc_desktop::launcher::{self, App, Launcher};
use svc_desktop::rack::{self, KeyPlace};

fstd::program!(run);

const TOOLTIP_MS: u64 = 600;
/// The launcher's well: 560 px wide, 30 % from the top.
const LAUNCHER_W: u32 = 560;
const LAUNCHER_ROW: u32 = 44;
const LAUNCHER_ROWS: usize = 6;

/// One of the desktop's windows: its surface, mapped, and its events.
struct Win {
    id: u32,
    events: Channel,
    px: *mut u32,
    w: u32,
    h: u32,
    _vmo: Handle,
}

impl Win {
    fn surface(&mut self) -> Surface<'_> {
        // SAFETY: the surface VMO maps w × h u32s, ours to write, for the window's life.
        let px = unsafe { core::slice::from_raw_parts_mut(self.px, (self.w * self.h) as usize) };
        Surface { px, width: self.w, height: self.h, stride: self.w }
    }
}

/// The Rack's keys: the launcher, then the owner's apps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    Launcher,
    App(usize),
}

/// An app the desktop knows: its words, its service, its shortcut.
struct Known {
    app: App,
    shortcut: &'static str,
}

struct Desktop {
    stage: Client,
    screen: (u32, u32),
    theme: Theme,
    fonts: Fonts<'static>,
    bundles: Vec<(u32, Bundle<'static>)>,
    hw: Hardware<'static>,
    rack: Win,
    places: Vec<KeyPlace>,
    keys: Vec<Key>,
    known: Vec<Known>,
    /// Which known apps the image carries, and which are running.
    available: Vec<bool>,
    running: Vec<bool>,
    hovered: Option<usize>,
    hover_at: u64,
    /// A press on the hovered key: no tooltip for it until the pointer moves on.
    tip_done: bool,
    pressed: Option<usize>,
    tooltip: Option<Win>,
    launcher: Option<(Win, Launcher)>,
    shortcuts: Option<Channel>,
    clock: String,
    stamped: bool,
}

fn deadline() -> u64 {
    Instant::after(Duration::from_secs(2))
}

fn fonts() -> Result<Fonts<'static>, String> {
    let f = |file| fstd::service::font(file).map_err(|e| alloc::format!("{file}: {}", e.describe()));
    Fonts::new(FontData {
        regular: f("NotoSans-Regular.ttf")?,
        medium: f("NotoSans-Medium.ttf")?,
        semibold: f("NotoSans-SemiBold.ttf")?,
        mono: f("NotoSansMono-Regular.ttf")?,
        symbols: f("NotoSansSymbols2-Regular.ttf")?,
    })
    .map_err(|e| alloc::format!("the fonts: {e:?}"))
}

/// The hardware set's bundles, by scale.
fn bundles() -> Vec<(u32, Bundle<'static>)> {
    let mut out = Vec::new();
    for n in 1..=3u32 {
        if let Ok(bytes) = fstd::service::data(&alloc::format!("hardware:{n}x"))
            && let Ok(b) = Bundle::parse(bytes)
        {
            out.push((n, b));
        }
    }
    out
}

/// The set for `theme` at the bundle nearest `scale` (rounded up).
fn hardware(bundles: &[(u32, Bundle<'static>)], mode: Mode, scale: f32) -> Result<Hardware<'static>, String> {
    let want = (scale + 0.999) as u32;
    let (n, b) = bundles
        .iter()
        .find(|(n, _)| *n >= want)
        .or(bundles.last())
        .copied()
        .ok_or_else(|| String::from("no hardware bundle"))?;
    let theme = if mode == Mode::Light { "light" } else { "dark" };
    Hardware::load(n, |piece| b.get(&alloc::format!("{theme}/{piece}")))
}

fn rdtsc() -> u64 {
    // SAFETY: RDTSC is allowed in user mode (CR4.TSD is clear).
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// A window of stage's, its surface mapped.
fn window(stage: &mut Client, req: &st::CreateWindow) -> Result<Win, String> {
    let (made, mut hs) = stage
        .call::<_, st::Created>(st::CREATE_WINDOW, req, Vec::new(), deadline())
        .map_err(|e| alloc::format!("create_window: {e:?}"))?;
    if hs.len() != 2 {
        return Err(String::from("stage sent no surface"));
    }
    let events = Channel(hs.remove(1));
    let vmo = hs.remove(0);
    let len = (req.w as u64 * req.h as u64 * 4).next_multiple_of(4096);
    let at = fstd::sys::vm_map(well_known::SELF_ASPACE, vmo.raw(), 0, len, 0, perm::READ | perm::WRITE)
        .map_err(|e| String::from(e.describe()))?;
    Ok(Win { id: made.window_id, events, px: at as *mut u32, w: req.w, h: req.h, _vmo: vmo })
}

/// Shows the window's surface. It's drawn opaque (0x00RRGGBB, as the
/// drawing code writes); a surface is premultiplied BGRA, so every pixel gets
/// its full alpha first, or stage would see straight through it.
fn present(stage: &mut Client, w: &mut Win) {
    for p in w.surface().px.iter_mut() {
        *p |= 0xFF00_0000;
    }
    let _ =
        stage.call::<_, ()>(st::PRESENT, &st::Present { window_id: w.id, damage: Vec::new() }, Vec::new(), deadline());
}

fn close(stage: &mut Client, w: Win) {
    let _ = stage.call::<_, ()>(st::CLOSE_WINDOW, &st::ById { window_id: w.id }, Vec::new(), deadline());
}

/// A key's legend, drawn in its centre until the icons are rendered.
fn legend(s: &mut Surface, key: Key, known: &[Known], cx: f32, cy: f32, scale: f32, ink: Rgba) {
    let k = scale;
    match key {
        Key::Launcher => {
            for dy in [-6.0, 0.0, 6.0] {
                for dx in [-6.0, 0.0, 6.0] {
                    draw::disc(s, cx + dx * k, cy + dy * k, 1.7 * k, ink);
                }
            }
        }
        Key::App(i) => match known[i].app.service.as_str() {
            "terminal" => {
                let w = 1.9 * k;
                draw::line(s, cx - 8.0 * k, cy - 5.0 * k, cx - 3.0 * k, cy, w, ink);
                draw::line(s, cx - 3.0 * k, cy, cx - 8.0 * k, cy + 5.0 * k, w, ink);
                draw::line(s, cx - 1.0 * k, cy + 6.0 * k, cx + 8.0 * k, cy + 6.0 * k, w, ink);
            }
            _ => {
                // Monitor: three meter bars.
                for (dx, h) in [(-6.0, 7.0), (0.0, 13.0), (6.0, 10.0)] {
                    let x = cx + dx * k;
                    draw::line(s, x, cy + 7.0 * k, x, cy + 7.0 * k - h * k, 3.0 * k, ink);
                }
            }
        },
    }
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    match Desktop::open() {
        Ok(mut d) => d.serve(),
        Err(e) => {
            fstd::error!("desktop: {e}");
            1
        }
    }
}

impl Desktop {
    fn open() -> Result<Desktop, String> {
        let ch = fstd::service::connect("stage").map_err(|e| alloc::format!("stage: {e:?}"))?;
        let mut stage = Client::new(ch);
        let (scr, _) = stage
            .call::<_, st::ScreenInfo>(st::SCREEN, &(), Vec::new(), deadline())
            .map_err(|e| alloc::format!("the screen: {e:?}"))?;
        let mode = if scr.theme == st::Theme::Light { Mode::Light } else { Mode::Dark };
        let theme = Theme::new(mode, scr.scale_pct as f32 / 100.0);
        let bundles = bundles();
        let hw = hardware(&bundles, mode, theme.scale)?;
        let shortcuts = stage
            .call::<_, st::Subscribed>(st::SUBSCRIBE_SHORTCUTS, &(), Vec::new(), deadline())
            .ok()
            .and_then(|(_, mut hs)| (!hs.is_empty()).then(|| Channel(hs.remove(0))));
        let known = alloc::vec![
            Known {
                app: App {
                    name: String::from("Terminal"),
                    description: String::from("A command line, with Freally Shell."),
                    service: String::from("terminal"),
                },
                shortcut: "Super+T",
            },
            Known {
                app: App {
                    name: String::from("Monitor"),
                    description: String::from("Everything running, how the boot went, and what crashed."),
                    service: String::from("monitor"),
                },
                shortcut: "Super+M",
            },
        ];
        let mut keys = alloc::vec![Key::Launcher];
        for (i, k) in known.iter().enumerate() {
            if rack::APPS.contains(&k.app.service.as_str()) {
                keys.push(Key::App(i));
            }
        }
        let rack_h = theme.px(rack::HEIGHT);
        let rack = window(
            &mut stage,
            &st::CreateWindow {
                title: String::from("Rack"),
                w: scr.w,
                h: rack_h,
                min_w: 0,
                min_h: 0,
                kind: st::Kind::Panel,
                x: 0,
                y: (scr.h - rack_h) as i32,
            },
        )?;
        let n = known.len();
        let mut d = Desktop {
            stage,
            screen: (scr.w, scr.h),
            theme,
            fonts: fonts()?,
            bundles,
            hw,
            rack,
            places: rack::keys(keys.len(), theme.scale),
            keys,
            known,
            available: alloc::vec![false; n],
            running: alloc::vec![false; n],
            hovered: None,
            hover_at: 0,
            tip_done: false,
            pressed: None,
            tooltip: None,
            launcher: None,
            shortcuts,
            clock: String::new(),
            stamped: false,
        };
        d.services();
        d.draw_rack();
        Ok(d)
    }

    /// Which known apps the image carries and which run, from conductor.
    fn services(&mut self) {
        if let Ok(list) = fstd::service::ask::<_, c::Services>(c::LIST_SERVICES, &()) {
            for (i, k) in self.known.iter().enumerate() {
                let s = list.services.iter().find(|s| s.name == k.app.service);
                self.available[i] = s.is_some();
                self.running[i] = s.is_some_and(|s| s.state == c::ServiceState::Running);
            }
        }
    }

    fn draw_rack(&mut self) {
        let t = self.theme;
        let face = t.colour(Tok::Surface).xrgb();
        let (w, h) = (self.rack.w, self.rack.h);
        let places = self.places.clone();
        let keys = self.keys.clone();
        let (hovered, pressed) = (self.hovered, self.pressed);
        let avail: Vec<bool> = keys.iter().map(|k| self.key_available(*k)).collect();
        let lit: Vec<bool> = keys.iter().map(|k| matches!(k, Key::App(i) if self.running[*i])).collect();
        self.clock = clock::twelve_hour(fstd::time::utc_ns());
        let text = self.clock.clone();
        let (tw, line) = self.fonts.measure(t, TextStyle::Clock, &text);
        let mut s = self.rack.surface();
        fill(&mut s, Rect::new(0, 0, w, h), face);
        fill(&mut s, Rect::new(0, 0, w, t.px(1).max(1)), t.colour(Tok::Border).xrgb());
        for (i, (k, p)) in keys.iter().zip(&places).enumerate() {
            let look = if pressed == Some(i) {
                KeyLook::Press(5)
            } else if hovered == Some(i) {
                KeyLook::Hover
            } else {
                KeyLook::Rest
            };
            let r = Rect::new(p.x, p.y, p.size, p.size);
            paint_key(&mut s, &self.hw, r, look);
            let ink = t.colour(if hovered == Some(i) && avail[i] { Tok::Text } else { Tok::Text2 });
            let (cx, cy) = (p.x as f32 + p.size as f32 / 2.0, p.y as f32 + p.size as f32 / 2.0);
            legend(&mut s, *k, &self.known, cx, cy, t.scale, ink);
            if !avail[i] {
                // 40 %: the key fades toward the faceplate.
                let m = t.px(4) as i32;
                fade(&mut s, Rect::new(p.x - m, p.y - m, p.size + 2 * m as u32, p.size + 2 * m as u32), face, 0.6);
            }
            let lamp = &self.hw.lamp[0];
            let state = if lit[i] { Lamp::On } else { Lamp::Off };
            paint_lamp(&mut s, &self.hw, p.led.0 - lamp.width as i32 / 2, p.led.1 - lamp.height as i32 / 2, state);
        }
        // The clock, in its display well.
        let pad = t.px(rack::CLOCK_PAD) as i32;
        let inner = t.px(12) as i32;
        let well_h = (line + t.px(4)).min(h - t.px(8));
        let well =
            Rect::new(w as i32 - pad - tw as i32 - 2 * inner, ((h - well_h) / 2) as i32, tw + 2 * inner as u32, well_h);
        draw::round_rect(&mut s, well, 6.0 * t.scale, t.colour(Tok::Border));
        draw::round_rect(
            &mut s,
            Rect::new(well.x + 1, well.y + 1, well.w - 2, well.h - 2),
            5.0 * t.scale,
            t.colour(Tok::Bg),
        );
        words(
            &mut s,
            &mut self.fonts,
            t,
            TextStyle::Clock,
            (well.x + inner, well.y + (well.h as i32 - line as i32) / 2),
            &text,
            Tok::PrimaryInk,
        );
        present(&mut self.stage, &mut self.rack);
        if !self.stamped {
            self.stamped = true;
            if let Err(e) = fstd::service::stamp(14, rdtsc()) {
                fstd::warn!("desktop: S4 not stamped: {e:?}");
            }
            fstd::info!("desktop: the Rack is up, {} keys, {}", self.keys.len(), self.clock);
        }
    }

    fn key_available(&self, k: Key) -> bool {
        match k {
            Key::Launcher => true,
            Key::App(i) => self.available[i],
        }
    }

    fn tooltip_text(&self, i: usize) -> String {
        match self.keys[i] {
            Key::Launcher => String::from("Apps · Super"),
            Key::App(a) if !self.available[a] => String::from("Not available yet."),
            Key::App(a) => alloc::format!("{} · {}", self.known[a].app.name, self.known[a].shortcut),
        }
    }

    fn show_tooltip(&mut self, i: usize) {
        self.hide_tooltip();
        let t = self.theme;
        let text = self.tooltip_text(i);
        let (tw, line) = self.fonts.measure(t, TextStyle::Small, &text);
        let pad = t.px(8);
        let (w, h) = (tw + 2 * pad, line + pad);
        let p = self.places[i];
        let x = (p.x + p.size as i32 / 2 - w as i32 / 2).clamp(0, (self.screen.0 - w) as i32);
        let y = (self.screen.1 - self.rack.h) as i32 - h as i32 - t.px(6) as i32;
        let req = st::CreateWindow {
            title: String::from("Tooltip"),
            w,
            h,
            min_w: 0,
            min_h: 0,
            kind: st::Kind::Tooltip,
            x,
            y,
        };
        let Ok(mut win) = window(&mut self.stage, &req) else { return };
        let mut s = win.surface();
        fill(&mut s, Rect::new(0, 0, w, h), t.colour(Tok::Border).xrgb());
        fill(&mut s, Rect::new(1, 1, w - 2, h - 2), t.colour(Tok::Surface2).xrgb());
        words(&mut s, &mut self.fonts, t, TextStyle::Small, (pad as i32, (pad / 2) as i32), &text, Tok::Text);
        present(&mut self.stage, &mut win);
        self.tooltip = Some(win);
    }

    fn hide_tooltip(&mut self) {
        if let Some(w) = self.tooltip.take() {
            close(&mut self.stage, w);
        }
    }

    /// The apps the launcher lists: the known ones the image carries.
    fn apps(&self) -> Vec<App> {
        self.known.iter().zip(&self.available).filter(|(_, a)| **a).map(|(k, _)| k.app.clone()).collect()
    }

    fn open_launcher(&mut self) {
        if self.launcher.is_some() {
            return;
        }
        self.hide_tooltip();
        let t = self.theme;
        let w = t.px(LAUNCHER_W);
        let h = t.px(56 + LAUNCHER_ROW * LAUNCHER_ROWS as u32 + 16);
        let x = (self.screen.0 as i32 - w as i32) / 2;
        let y = (self.screen.1 as f32 * 0.3) as i32;
        let req =
            st::CreateWindow { title: String::from("Launcher"), w, h, min_w: 0, min_h: 0, kind: st::Kind::Popup, x, y };
        match window(&mut self.stage, &req) {
            Ok(win) => {
                self.launcher = Some((win, Launcher::default()));
                self.draw_launcher();
                fstd::info!("desktop: the launcher is open");
            }
            Err(e) => fstd::warn!("desktop: no launcher: {e}"),
        }
    }

    fn close_launcher(&mut self) {
        if let Some((w, _)) = self.launcher.take() {
            close(&mut self.stage, w);
            fstd::info!("desktop: the launcher is closed");
        }
    }

    fn draw_launcher(&mut self) {
        let apps = self.apps();
        let t = self.theme;
        let Some((win, l)) = self.launcher.as_mut() else { return };
        let (w, h) = (win.w, win.h);
        let matches = launcher::filter(&apps, &l.query);
        let (query, selected) = (l.query.clone(), l.selected);
        let mut s = win.surface();
        fill(&mut s, Rect::new(0, 0, w, h), t.colour(Tok::Border).xrgb());
        fill(&mut s, Rect::new(1, 1, w - 2, h - 2), t.colour(Tok::Surface).xrgb());
        // The well: what's typed, with its caret.
        let pad = t.px(16) as i32;
        let well = Rect::new(pad, pad, w - 2 * pad as u32, t.px(40));
        draw::round_rect(&mut s, well, 6.0 * t.scale, t.colour(Tok::Border));
        draw::round_rect(
            &mut s,
            Rect::new(well.x + 1, well.y + 1, well.w - 2, well.h - 2),
            5.0 * t.scale,
            t.colour(Tok::Bg),
        );
        let (_, line, _) = t.text(TextStyle::Subtitle);
        let ty = well.y + (well.h as i32 - line as i32) / 2;
        let shown = if query.is_empty() { "Type to find an app" } else { query.as_str() };
        words(
            &mut s,
            &mut self.fonts,
            t,
            TextStyle::Subtitle,
            (well.x + pad / 2, ty),
            shown,
            if query.is_empty() { Tok::Text3 } else { Tok::Text },
        );
        let caret_x = well.x
            + pad / 2
            + if query.is_empty() { 0 } else { self.fonts.measure(t, TextStyle::Subtitle, &query).0 as i32 + 1 };
        fill(&mut s, Rect::new(caret_x, ty + 2, t.px(2).max(1), line - 4), t.colour(Tok::PrimaryInk).xrgb());
        // The matches, the chosen one raised.
        let row_h = t.px(LAUNCHER_ROW) as i32;
        let top = well.y + well.h as i32 + t.px(8) as i32;
        if matches.is_empty() {
            let msg = if query.is_empty() {
                String::from("No apps yet.")
            } else {
                alloc::format!("No app called '{query}'.")
            };
            words(&mut s, &mut self.fonts, t, TextStyle::Body, (pad, top + t.px(8) as i32), &msg, Tok::Text2);
        }
        for (row, &i) in matches.iter().take(LAUNCHER_ROWS).enumerate() {
            let y = top + row as i32 * row_h;
            if row == selected {
                draw::round_rect(
                    &mut s,
                    Rect::new(pad / 2, y, w - pad as u32, row_h as u32),
                    6.0 * t.scale,
                    t.colour(Tok::Surface2),
                );
            }
            words(&mut s, &mut self.fonts, t, TextStyle::Body, (pad, y + t.px(2) as i32), &apps[i].name, Tok::Text);
            words(
                &mut s,
                &mut self.fonts,
                t,
                TextStyle::Small,
                (pad, y + t.px(21) as i32),
                &apps[i].description,
                Tok::Text2,
            );
        }
        if let Some((win, _)) = self.launcher.as_mut() {
            present(&mut self.stage, win);
        }
    }

    /// Starts (or brings forward) the app behind a known entry.
    fn start(&mut self, i: usize) {
        if !self.available[i] {
            return;
        }
        let service = self.known[i].app.service.clone();
        match fstd::service::connect_within(&service, Duration::from_secs(5)) {
            Ok(_) => fstd::info!("desktop: started {service}"),
            Err(e) => fstd::warn!("desktop: couldn't start {service}: {e:?}"),
        }
        self.services();
        self.draw_rack();
    }

    fn activate(&mut self, i: usize) {
        match self.keys[i] {
            Key::Launcher => {
                if self.launcher.is_some() {
                    self.close_launcher();
                } else {
                    self.open_launcher();
                }
            }
            Key::App(a) => self.start(a),
        }
    }

    fn rack_event(&mut self, ordinal: u32, p: &[u8]) {
        match ordinal {
            st::EV_POINTER => {
                let Ok(e) = fwire::from_payload::<st::Pointer>(p) else { return };
                let at = if e.kind == st::PointerKind::Leave { None } else { rack::key_at(&self.places, e.x, e.y) };
                if at != self.hovered {
                    self.hovered = at;
                    self.hover_at = Instant::now().as_ns();
                    self.tip_done = false;
                    self.hide_tooltip();
                    self.draw_rack();
                }
                match e.kind {
                    st::PointerKind::Down if e.buttons & 1 != 0 => {
                        self.pressed = at;
                        self.tip_done = true;
                        self.hide_tooltip();
                        self.draw_rack();
                    }
                    st::PointerKind::Up => {
                        let was = self.pressed.take();
                        self.draw_rack();
                        if let Some(i) = was.filter(|&i| Some(i) == at) {
                            self.activate(i);
                        }
                    }
                    _ => {}
                }
            }
            st::EV_THEME => {
                if let Ok(t) = fwire::from_payload::<st::SetTheme>(p) {
                    self.theme.mode = if t.theme == st::Theme::Light { Mode::Light } else { Mode::Dark };
                    self.reload_art();
                }
            }
            st::EV_SCALE => {
                if let Ok(sc) = fwire::from_payload::<st::SetScale>(p) {
                    self.theme.scale = sc.pct as f32 / 100.0;
                    self.places = rack::keys(self.keys.len(), self.theme.scale);
                    self.reload_art();
                }
            }
            _ => {}
        }
    }

    fn reload_art(&mut self) {
        match hardware(&self.bundles, self.theme.mode, self.theme.scale) {
            Ok(hw) => self.hw = hw,
            Err(e) => fstd::warn!("desktop: {e}"),
        }
        self.draw_rack();
        if self.launcher.is_some() {
            self.draw_launcher();
        }
    }

    fn launcher_event(&mut self, ordinal: u32, p: &[u8]) {
        match ordinal {
            st::EV_KEY => {
                let Ok(k) = fwire::from_payload::<st::Key>(p) else { return };
                if !k.down {
                    return;
                }
                let apps = self.apps();
                let Some((_, l)) = self.launcher.as_mut() else { return };
                match l.key(&apps, k.usage, k.ch) {
                    launcher::Outcome::Changed => self.draw_launcher(),
                    launcher::Outcome::Close => self.close_launcher(),
                    launcher::Outcome::Start(i) => {
                        let service = apps[i].service.clone();
                        self.close_launcher();
                        if let Some(k) = self.known.iter().position(|k| k.app.service == service) {
                            self.start(k);
                        }
                    }
                    launcher::Outcome::Nothing => {}
                }
            }
            st::EV_FOCUS => {
                // Clicking elsewhere closes it, as a launcher should.
                if let Ok(f) = fwire::from_payload::<st::Focus>(p)
                    && !f.focused
                {
                    self.close_launcher();
                }
            }
            st::EV_CLOSE_REQUESTED => self.close_launcher(),
            _ => {}
        }
    }

    fn shortcut(&mut self, p: &[u8]) {
        let Ok(e) = fwire::from_payload::<st::ShortcutEvent>(p) else { return };
        match e.shortcut {
            st::Shortcut::Launcher => {
                if self.launcher.is_some() {
                    self.close_launcher();
                } else {
                    self.open_launcher();
                }
            }
            st::Shortcut::Monitor | st::Shortcut::Terminal => {
                let want = if e.shortcut == st::Shortcut::Monitor { "monitor" } else { "terminal" };
                if let Some(i) = self.known.iter().position(|k| k.app.service == want) {
                    if self.available[i] {
                        self.start(i);
                    } else {
                        fstd::info!("desktop: {want} isn't in this image yet");
                    }
                }
            }
        }
    }

    /// Reads every message waiting on `ch`, each as (ordinal, payload).
    fn drain(ch: &Channel) -> Result<Vec<(u32, Vec<u8>)>, FErr> {
        let mut out = Vec::new();
        loop {
            match ch.try_recv() {
                Ok(m) => {
                    if let Ok((h, p)) = fwire::parse(&m.bytes) {
                        out.push((h.ordinal, p.to_vec()));
                    }
                }
                Err(FErr::ShouldWait) => return Ok(out),
                Err(e) => return Err(e),
            }
        }
    }

    fn serve(&mut self) -> i64 {
        loop {
            let mut items = alloc::vec![[self.rack.events.0.raw(), signal::READABLE | signal::PEER_CLOSED]];
            if let Some(ch) = &self.shortcuts {
                items.push([ch.0.raw(), signal::READABLE]);
            }
            if let Some((w, _)) = &self.launcher {
                items.push([w.events.0.raw(), signal::READABLE]);
            }
            // Wake for the tooltip's 600 ms, or the clock's next minute.
            let now = Instant::now().as_ns();
            let mut until = now + clock::until_next_minute(fstd::time::utc_ns());
            if self.hovered.is_some() && self.tooltip.is_none() && self.pressed.is_none() && !self.tip_done {
                until = until.min(self.hover_at + TOOLTIP_MS * 1_000_000);
            }
            let _ = fstd::sys::wait_many(&items, until);
            match Self::drain(&self.rack.events) {
                Ok(evs) => evs.into_iter().for_each(|(o, p)| self.rack_event(o, &p)),
                Err(_) => {
                    fstd::error!("desktop: stage is gone");
                    return 1;
                }
            }
            if let Some(evs) = self.shortcuts.as_ref().and_then(|ch| Self::drain(ch).ok()) {
                evs.into_iter().filter(|(o, _)| *o == st::EV_SHORTCUT).for_each(|(_, p)| self.shortcut(&p));
            }
            if let Some(evs) = self.launcher.as_ref().and_then(|(w, _)| Self::drain(&w.events).ok()) {
                evs.into_iter().for_each(|(o, p)| self.launcher_event(o, &p));
            }
            let now = Instant::now().as_ns();
            if let Some(i) = self.hovered
                && self.tooltip.is_none()
                && self.pressed.is_none()
                && !self.tip_done
                && now >= self.hover_at + TOOLTIP_MS * 1_000_000
            {
                self.show_tooltip(i);
            }
            if clock::twelve_hour(fstd::time::utc_ns()) != self.clock {
                self.services();
                self.draw_rack();
            }
        }
    }
}
