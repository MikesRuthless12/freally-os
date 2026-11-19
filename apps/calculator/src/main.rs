// SPDX-License-Identifier: Apache-2.0
//! Calculator, the program (TASK-112, FR-054): the modes and the toggles
//! across the top, the display (the sum, the answer, its value while it's
//! typed), and the pad of hardware-set keys, or the history in its place.
//! The keyboard types the sum; Enter works it out, Backspace takes a
//! character back, Escape clears, Ctrl+C copies the answer and Ctrl+V
//! pastes.
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use app_calculator::{Calc, Key, Mode, SCIENTIFIC, STANDARD, key};
use fapp::{Look, Window, inside};
use fcore_abi::signal;
use fdecimal::expr::Angle;
use fimage::bundle::Bundle;
use fstd::time::{Duration, Instant};
use fui::controls::paint::paint_key;
use fui::draw;
use fui::hardware::{Hardware, KeyLook};
use fui::layout::Rect;
use fui::theme::{Mode as Theme, TextStyle, Tok};
use fwire::proto::ax;
use fwire::proto::stage as st;

fstd::program!(run);

/// The window, standard and scientific (unscaled).
const STANDARD_SIZE: (u32, u32) = (340, 560);
const SCIENTIFIC_SIZE: (u32, u32) = (660, 560);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    Key(Key),
    Mode(Mode),
    Angle,
    History,
    Recall(usize),
    ClearHistory,
}

struct App {
    win: Window,
    calc: Calc,
    /// The hardware set at the window's scale and theme (`None`: drawn plain).
    hw: Option<Hardware<'static>>,
    history_open: bool,
    hits: Vec<(Rect, Hit)>,
    hovered: Option<Hit>,
    pressed: Option<Hit>,
}

/// The hardware set for the theme at the scale (TASK-090A: 1:1, never scaled).
fn hardware(mode: Theme, pct: u16) -> Option<Hardware<'static>> {
    let b = fstd::service::data(&format!("hardware:{pct}")).ok().and_then(|b| Bundle::parse(b).ok())?;
    let theme = if mode == Theme::Light { "light" } else { "dark" };
    Hardware::load(pct as u32, |piece| b.get(&format!("{theme}/{piece}"))).ok()
}

impl App {
    fn load_hardware(&mut self) {
        let t = self.win.theme;
        fimage::guard::expect_scale((t.scale * 100.0 + 0.5) as u32);
        self.hw = hardware(t.mode, (t.scale * 100.0 + 0.5) as u16);
        if self.hw.is_none() {
            fstd::warn!("calculator: no hardware set at this scale: the keys are drawn plain");
        }
    }

    /// A toggle in the bar: gray off, white off under the pointer, green on
    /// (the fleet's rule). Its rectangle.
    #[allow(clippy::too_many_arguments)]
    fn toggle(&mut self, (x, y): (i32, i32), label: &str, on: bool, hit: Hit, role: u8, testid: &str) -> Rect {
        let t = self.win.theme;
        let (lw, lh) = self.win.measure(TextStyle::Body, label);
        let r = Rect::new(x, y, lw + 2 * t.px(12), t.px(32));
        let hovered = self.hovered == Some(hit);
        let ink = if on {
            Tok::PrimaryInk
        } else if hovered {
            Tok::Text
        } else {
            Tok::Text2
        };
        if on || hovered {
            draw::round_rect(&mut self.win.surface(), r, 6.0 * t.scale, t.colour(Tok::Surface2));
        }
        self.win.say((r.x + t.px(12) as i32, r.y + (r.h as i32 - lh as i32) / 2), TextStyle::Body, label, ink);
        let state = if on { if role == ax::TAB { ax::SELECTED } else { ax::CHECKED } } else { 0 }
            | if hovered { ax::HOVERED } else { 0 };
        let mut n = fstd::ax::node(ax::ROOT, role, label, state, (r.x, r.y, r.w, r.h));
        n.testid = String::from(testid);
        self.win.nodes.push(n);
        self.hits.push((r, hit));
        r
    }

    fn draw(&mut self) {
        let t = self.win.theme;
        self.win.nodes.clear();
        self.hits.clear();
        self.win.ground(false);
        let (w, h) = (self.win.w, self.win.h);
        let pad = t.px(12) as i32;
        // The bar.
        let mut x = pad;
        for (m, label, id) in [
            (Mode::Standard, "Standard", "calc-mode-standard"),
            (Mode::Scientific, "Scientific", "calc-mode-scientific"),
        ] {
            let r = self.toggle((x, pad), label, self.calc.mode == m, Hit::Mode(m), ax::TAB, id);
            x = r.x + r.w as i32 + t.px(4) as i32;
        }
        let history_w = self.win.measure(TextStyle::Body, "History").0 + 2 * t.px(12);
        let mut right = w as i32 - pad - history_w as i32;
        self.toggle((right, pad), "History", self.history_open, Hit::History, ax::SWITCH, "calc-history");
        if self.calc.mode == Mode::Scientific {
            let label = if self.calc.angle == Angle::Degrees { "Degrees" } else { "Radians" };
            let aw = self.win.measure(TextStyle::Body, label).0 + 2 * t.px(12);
            right -= aw as i32 + t.px(4) as i32;
            self.toggle((right, pad), label, self.calc.angle == Angle::Radians, Hit::Angle, ax::SWITCH, "calc-angle");
            if let Some(n) = self.win.nodes.last_mut() {
                n.value = label.to_ascii_lowercase();
            }
        }
        // The display: the sum (once answered), the big line, the value as typed.
        let top = pad + t.px(32) as i32 + t.px(10) as i32;
        let disp = Rect::new(pad, top, w - 2 * pad as u32, t.px(116));
        {
            let mut s = self.win.surface();
            draw::round_rect(&mut s, disp, 8.0 * t.scale, t.colour(Tok::Border));
            draw::round_rect(
                &mut s,
                Rect::new(disp.x + 1, disp.y + 1, disp.w - 2, disp.h - 2),
                7.0 * t.scale,
                t.colour(Tok::Bg),
            );
        }
        let inner = t.px(14) as i32;
        let room = disp.w - 2 * inner as u32;
        let sum = if self.calc.answered() { format!("{} =", self.calc.entry) } else { String::new() };
        let preview = self.calc.preview().map(|v| format!("= {v}")).unwrap_or_default();
        let shown = self.calc.shown();
        let small_h = t.text(TextStyle::Small).1 as i32;
        self.line(disp, inner, disp.y + inner, TextStyle::Small, &sum, Tok::Text2, "calc-sum");
        let big = [TextStyle::Display, TextStyle::Title, TextStyle::Subtitle]
            .into_iter()
            .find(|&st| self.win.measure(st, &shown).0 <= room)
            .unwrap_or(TextStyle::Body);
        let big_h = t.text(big).1 as i32;
        let big_y = disp.y + inner + small_h + (disp.h as i32 - 2 * inner - 2 * small_h - big_h) / 2;
        let error = matches!(self.calc.answer, Some(Err(_))) && self.calc.answered();
        self.line(disp, inner, big_y, big, &shown, if error { Tok::Error } else { Tok::Text }, "calc-display");
        self.line(
            disp,
            inner,
            disp.y + disp.h as i32 - inner - small_h,
            TextStyle::Small,
            &preview,
            Tok::Text2,
            "calc-preview",
        );
        // The pad, or the history in its place.
        let area_y = disp.y + disp.h as i32 + t.px(12) as i32;
        let area = Rect::new(pad, area_y, w - 2 * pad as u32, (h as i32 - area_y - pad).max(0) as u32);
        if self.history_open {
            self.draw_history(area);
        } else {
            self.draw_pad(area);
        }
        self.win.present();
    }

    /// One line of the display, right-aligned (its end kept if too long).
    #[allow(clippy::too_many_arguments)]
    fn line(&mut self, disp: Rect, inner: i32, y: i32, style: TextStyle, text: &str, tok: Tok, testid: &str) {
        let room = disp.w - 2 * inner as u32;
        let mut shown = String::from(text);
        while self.win.measure(style, &shown).0 > room && shown.chars().count() > 1 {
            let rest: String = shown.chars().skip(2).collect();
            shown = format!("…{rest}");
        }
        let (lw, lh) = self.win.measure(style, &shown);
        let x = disp.x + disp.w as i32 - inner - lw as i32;
        self.win.say((x, y), style, &shown, tok);
        let mut n = fstd::ax::node(ax::ROOT, ax::TEXT, text, 0, (x, y, lw, lh));
        n.testid = String::from(testid);
        n.value = String::from(text);
        self.win.nodes.push(n);
    }

    fn key_look(&self, hit: Hit) -> KeyLook {
        if self.pressed == Some(hit) && self.hovered == Some(hit) {
            KeyLook::Press(5)
        } else if self.hovered == Some(hit) {
            KeyLook::Hover
        } else {
            KeyLook::Rest
        }
    }

    /// A hardware-set key with its legend (plain where the set isn't here).
    fn key(&mut self, r: Rect, label: &str, hit: Hit, legend: Tok, testid: &str) {
        let look = self.key_look(hit);
        match &self.hw {
            Some(hw) => {
                paint_key(&mut self.win.surface(), hw, r, look);
                let style = if label.chars().count() == 1 { TextStyle::Subtitle } else { TextStyle::Body };
                let (lw, lh) = self.win.measure(style, label);
                let sink = if matches!(look, KeyLook::Press(_)) { self.win.theme.px(1) as i32 } else { 0 };
                self.win.say(
                    (r.x + (r.w as i32 - lw as i32) / 2, r.y + (r.h as i32 - lh as i32) / 2 + sink),
                    style,
                    label,
                    legend,
                );
                let state = if matches!(look, KeyLook::Press(_)) { ax::PRESSED } else { 0 };
                let mut n = fstd::ax::node(ax::ROOT, ax::BUTTON, label, state, (r.x, r.y, r.w, r.h));
                n.testid = String::from(testid);
                self.win.nodes.push(n);
            }
            None => {
                let l = Look {
                    focused: false,
                    hovered: matches!(look, KeyLook::Hover),
                    pressed: matches!(look, KeyLook::Press(_)),
                };
                self.win.key(r, label, legend == Tok::PrimaryInk, l, testid);
            }
        }
        self.hits.push((r, hit));
    }

    fn draw_pad(&mut self, area: Rect) {
        let t = self.win.theme;
        let gap = t.px(8);
        let rows: Vec<Vec<Key>> = match self.calc.mode {
            Mode::Standard => STANDARD.iter().map(|r| r.to_vec()).collect(),
            Mode::Scientific => SCIENTIFIC.iter().zip(STANDARD.iter()).map(|(a, b)| [*a, *b].concat()).collect(),
        };
        let cols = rows[0].len() as u32;
        let kw = (area.w.saturating_sub(gap * (cols - 1))) / cols;
        let kh = (area.h.saturating_sub(gap * (rows.len() as u32 - 1))) / rows.len() as u32;
        for (ri, row) in rows.iter().enumerate() {
            for (ci, &k) in row.iter().enumerate() {
                let r =
                    Rect::new(area.x + ci as i32 * (kw + gap) as i32, area.y + ri as i32 * (kh + gap) as i32, kw, kh);
                let legend = match k {
                    Key::Equals => Tok::PrimaryInk,
                    Key::Digit(_) | Key::Point => Tok::Text,
                    _ => Tok::Text2,
                };
                self.key(r, k.label(), Hit::Key(k), legend, &format!("calc-key-{}", k.name()));
            }
        }
    }

    fn draw_history(&mut self, area: Rect) {
        let t = self.win.theme;
        let clear_h = t.px(44);
        let list = Rect::new(area.x, area.y, area.w, area.h.saturating_sub(clear_h + t.px(8)));
        let mut lb = fstd::ax::node(ax::ROOT, ax::LISTBOX, "History", 0, (list.x, list.y, list.w, list.h));
        lb.testid = String::from("calc-history-list");
        self.win.nodes.push(lb);
        if self.calc.history.is_empty() {
            let words = "No sums yet";
            let (ww, wh) = self.win.measure(TextStyle::Body, words);
            self.win.say(
                (list.x + (list.w as i32 - ww as i32) / 2, list.y + (list.h as i32 - wh as i32) / 2),
                TextStyle::Body,
                words,
                Tok::Text2,
            );
        }
        let row_h = t.px(56);
        let inner = t.px(12) as i32;
        let fit = (list.h / row_h) as usize;
        let rows: Vec<(String, String)> = self.calc.history.iter().take(fit).cloned().collect();
        for (i, (sum, answer)) in rows.iter().enumerate() {
            let r = Rect::new(list.x, list.y + (i as u32 * row_h) as i32, list.w, row_h - t.px(4));
            let hit = Hit::Recall(i);
            if self.hovered == Some(hit) {
                draw::round_rect(&mut self.win.surface(), r, 6.0 * t.scale, t.colour(Tok::Surface2));
            }
            let (sw, _) = self.win.measure(TextStyle::Small, sum);
            self.win.say(
                (r.x + r.w as i32 - inner - sw as i32, r.y + t.px(6) as i32),
                TextStyle::Small,
                sum,
                Tok::Text2,
            );
            let shown = format!("= {answer}");
            let (aw, ah) = self.win.measure(TextStyle::Subtitle, &shown);
            self.win.say(
                (r.x + r.w as i32 - inner - aw as i32, r.y + r.h as i32 - t.px(6) as i32 - ah as i32),
                TextStyle::Subtitle,
                &shown,
                Tok::Text,
            );
            let mut n = fstd::ax::node(ax::ROOT, ax::OPTION, &format!("{sum} = {answer}"), 0, (r.x, r.y, r.w, r.h));
            n.testid = format!("calc-history-{i}");
            n.value = answer.clone();
            self.win.nodes.push(n);
            self.hits.push((r, hit));
        }
        let cw = t.px(160).min(area.w);
        let clear = Rect::new(area.x + (area.w - cw) as i32 / 2, area.y + (area.h - clear_h) as i32, cw, clear_h);
        self.key(clear, "Clear history", Hit::ClearHistory, Tok::Text2, "calc-history-clear");
    }

    fn hit_at(&self, x: i32, y: i32) -> Option<Hit> {
        self.hits.iter().rev().find(|(r, _)| inside(*r, x, y)).map(|(_, h)| *h)
    }

    fn set_mode(&mut self, m: Mode) {
        if self.calc.mode == m {
            return;
        }
        self.calc.mode = m;
        let size = if m == Mode::Scientific { SCIENTIFIC_SIZE } else { STANDARD_SIZE };
        if let Err(e) = self.win.resize(size) {
            fstd::error!("calculator: {e}");
        }
        fstd::info!("calculator: {}", if m == Mode::Scientific { "scientific" } else { "standard" });
    }

    fn click(&mut self, h: Hit) {
        match h {
            Hit::Key(k) => self.calc.press(k),
            Hit::Mode(m) => self.set_mode(m),
            Hit::Angle => self.calc.toggle_angle(),
            Hit::History => self.history_open = !self.history_open,
            Hit::Recall(i) => {
                self.calc.recall(i);
                self.history_open = false;
            }
            Hit::ClearHistory => self.calc.history.clear(),
        }
    }

    fn copy(&mut self) {
        let text = self.calc.copy_text();
        let c = st::Clip { window_id: self.win.id, text };
        match self.win.stage.call::<_, ()>(st::SET_CLIPBOARD, &c, Vec::new(), fapp::deadline()) {
            Ok(_) => fstd::info!("calculator: copied"),
            Err(e) => fstd::warn!("calculator: copy: {e:?}"),
        }
    }

    fn paste(&mut self) {
        let by = st::ById { window_id: self.win.id };
        match self.win.stage.call::<_, st::ClipText>(st::CLIPBOARD, &by, Vec::new(), fapp::deadline()) {
            Ok((c, _)) => self.calc.paste(&c.text),
            Err(e) => fstd::warn!("calculator: paste: {e:?}"),
        }
    }

    /// One event; `true` when the Calculator closes.
    fn event(&mut self, ordinal: u32, p: &[u8]) -> bool {
        match ordinal {
            st::EV_KEY => {
                let Ok(k) = fwire::from_payload::<st::Key>(p) else { return false };
                if !k.down {
                    return false;
                }
                let ctrl = k.modifiers & key::CTRL != 0;
                match k.usage {
                    key::C if ctrl => self.copy(),
                    key::V if ctrl => self.paste(),
                    key::BACKSPACE => self.calc.press(Key::Back),
                    key::ESCAPE if self.history_open => self.history_open = false,
                    key::ESCAPE | key::DELETE => self.calc.press(Key::Clear),
                    key::ENTER | key::KP_ENTER => self.calc.press(Key::Equals),
                    _ if ctrl => return false,
                    _ => match char::from_u32(k.ch).filter(|c| *c != '\0') {
                        Some(c) => self.calc.typed(c),
                        None => return false,
                    },
                }
                self.draw();
                false
            }
            st::EV_POINTER => {
                let Ok(e) = fwire::from_payload::<st::Pointer>(p) else { return false };
                let was = (self.hovered, self.pressed);
                self.hovered = if e.kind == st::PointerKind::Leave { None } else { self.hit_at(e.x, e.y) };
                match e.kind {
                    st::PointerKind::Down if e.buttons & 1 != 0 => self.pressed = self.hovered,
                    st::PointerKind::Up => {
                        if let Some(h) = self.pressed.take().filter(|h| Some(*h) == self.hovered) {
                            self.click(h);
                            self.draw();
                            return false;
                        }
                    }
                    _ => {}
                }
                if (self.hovered, self.pressed) != was {
                    self.draw();
                }
                false
            }
            st::EV_CLOSE_REQUESTED => true,
            st::EV_THEME => {
                self.load_hardware();
                self.draw();
                false
            }
            st::EV_SCALE => {
                if let Err(e) = self.win.reopen() {
                    fstd::error!("calculator: opening again at the new scale: {e}");
                    return true;
                }
                self.load_hardware();
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
    let win = match Window::open("Calculator", st::Kind::Normal, STANDARD_SIZE) {
        Ok(w) => w,
        Err(e) => {
            fstd::error!("calculator: {e}");
            return 1;
        }
    };
    let mut app =
        App { win, calc: Calc::new(), hw: None, history_open: false, hits: Vec::new(), hovered: None, pressed: None };
    app.load_hardware();
    app.draw();
    fstd::info!("calculator: open");
    loop {
        let _ = app.win.events.0.wait(signal::READABLE | signal::PEER_CLOSED, Instant::after(Duration::from_secs(5)));
        let Ok(events) = app.win.events() else { return 0 };
        for (ordinal, p) in events {
            if app.event(ordinal, &p) {
                fstd::info!("calculator: closed");
                return 0;
            }
        }
    }
}
