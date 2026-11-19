// SPDX-License-Identifier: Apache-2.0
//! The copy window (TASK-105A, FR-085): every running copy, each with its
//! progress, words, Pause, Cancel and More details (the live speed graph,
//! in the fleet's pen); a taken name asks Replace, Skip or *Let me decide
//! for each file* (*Do this for all conflicts* beside them); a failure asks
//! Try again, Skip or Cancel. The program steps each running job in turn.

use alloc::string::String;
use alloc::vec::Vec;
use app_files::copy_dialog::{Clash, Phase, Run};
use app_session::import::size;
use fapp::{Look, Window, inside};
use fstd::time::Instant;
use fui::draw::{self, fill};
use fui::layout::Rect;
use fui::theme::{Mode, TextStyle, Tok};
use fwire::proto::ax;
use fwire::proto::stage as st;
use fwire::proto::vfs as v;

const SIZE: (u32, u32) = (600, 460);
const PAD: u32 = 20;

/// A running copy, and what it was asked to do (for Ctrl+Z after a move).
pub struct Live {
    pub run: Run,
    pub sources: Vec<String>,
    pub dest: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    Pause(usize),
    Cancel(usize),
    Details(usize),
    Replace(usize),
    Skip(usize),
    Decide(usize),
    All(usize),
    KeepSrc(usize, usize),
    KeepDst(usize, usize),
    Continue(usize),
    TryAgain(usize),
    SkipFailed(usize),
}

pub struct Copies {
    pub win: Window,
    pub lives: Vec<Live>,
    hits: Vec<(Rect, Hit)>,
    hovered: Option<Hit>,
    pressed: Option<Hit>,
    /// The keyboard's place among the keys (Tab, Shift+Tab; Enter presses).
    focus: usize,
    drawn_ns: u64,
}

fn node(role: u8, name: &str, state: u32, r: Rect, id: &str) -> ax::Node {
    let mut n = fstd::ax::node(ax::ROOT, role, name, state, (r.x, r.y, r.w, r.h));
    n.testid = String::from(id);
    n
}

impl Copies {
    pub fn open() -> Result<Copies, String> {
        let win = Window::open("Copying", st::Kind::Normal, SIZE)?;
        Ok(Copies { win, lives: Vec::new(), hits: Vec::new(), hovered: None, pressed: None, focus: 0, drawn_ns: 0 })
    }

    /// Whether any job wants stepping.
    pub fn busy(&self) -> bool {
        self.lives.iter().any(|l| l.run.phase == Phase::Running)
    }

    /// Each running job one step further. The finished ones, taken out.
    pub fn step(&mut self) -> Vec<Live> {
        let now = Instant::now().as_ns();
        for l in self.lives.iter_mut().filter(|l| l.run.phase == Phase::Running) {
            match fstd::fs::copy_step(l.run.job) {
                Ok(s) => l.run.update(&s, now),
                Err(e) => l.run.phase = Phase::Failed(e.describe()),
            }
        }
        let mut done = Vec::new();
        let mut i = 0;
        while i < self.lives.len() {
            if matches!(self.lives[i].run.phase, Phase::Done | Phase::Cancelled) {
                done.push(self.lives.remove(i));
            } else {
                i += 1;
            }
        }
        // At most every 50 ms (the windows' own frames are dearer than the copy).
        if !done.is_empty() || now >= self.drawn_ns + 50_000_000 {
            self.draw();
        }
        done
    }

    fn key_rect(&mut self, r: Rect, label: &str, primary: bool, hit: Hit, id: &str) {
        let order = self.hits.len();
        let look = Look {
            focused: self.focus == order,
            hovered: self.hovered == Some(hit),
            pressed: self.pressed == Some(hit) && self.hovered == Some(hit),
        };
        self.win.key(r, label, primary, look, id);
        self.hits.push((r, hit));
    }

    /// A row of keys from (x, y): their rectangles' right edge.
    fn keys(&mut self, x: i32, y: i32, keys: &[(&str, bool, Hit, String)]) -> i32 {
        let t = self.win.theme;
        let mut kx = x;
        for (label, primary, hit, id) in keys {
            let (lw, _) = self.win.measure(TextStyle::Body, label);
            let r = Rect::new(kx, y, lw + t.px(28), t.px(32));
            self.key_rect(r, label, *primary, *hit, id);
            kx += (r.w + t.px(8)) as i32;
        }
        kx
    }

    fn checkbox(&mut self, r: Rect, on: bool, hit: Hit, label: &str, id: &str) {
        let t = self.win.theme;
        let order = self.hits.len();
        let hovered = self.hovered == Some(hit);
        let b = Rect::new(r.x, r.y + (r.h as i32 - t.px(18) as i32) / 2, t.px(18), t.px(18));
        {
            let mut s = self.win.surface();
            let edge = if self.focus == order {
                Tok::Focus
            } else if hovered {
                Tok::Text
            } else {
                Tok::Border
            };
            draw::round_rect(&mut s, b, 4.0 * t.scale, t.colour(edge));
            let inner = Rect::new(b.x + 2, b.y + 2, b.w - 4, b.h - 4);
            draw::round_rect(&mut s, inner, 3.0 * t.scale, t.colour(if on { Tok::Primary } else { Tok::Bg }));
        }
        let (_, lh) = self.win.measure(TextStyle::Small, label);
        self.win.say(
            (b.x + b.w as i32 + t.px(8) as i32, r.y + (r.h as i32 - lh as i32) / 2),
            TextStyle::Small,
            label,
            Tok::Text2,
        );
        let state = if on { ax::CHECKED } else { 0 } | if self.focus == order { ax::FOCUSED } else { 0 };
        self.win.nodes.push(node(ax::SWITCH, label, state, r, id));
        self.hits.push((r, hit));
    }

    pub fn draw(&mut self) {
        let t = self.win.theme;
        let (w, h) = (self.win.w, self.win.h);
        self.drawn_ns = Instant::now().as_ns();
        self.hits.clear();
        self.win.ground(false);
        let pad = t.px(PAD) as i32;
        let inner = w - 2 * pad as u32;
        let mut y = pad;
        let lives: Vec<(Run, usize)> = self.lives.iter().enumerate().map(|(i, l)| (l.run.clone(), i)).collect();
        for (r, i) in lives {
            let job = r.job;
            let top = y;
            self.win.say((pad, y), TextStyle::Subtitle, &r.title, Tok::Text);
            y += t.px(30) as i32;
            // The bar.
            let bar = Rect::new(pad, y, inner, t.px(8));
            {
                let mut s = self.win.surface();
                draw::round_rect(&mut s, bar, bar.h as f32 / 2.0, t.colour(Tok::MeterOff));
                let fw = ((inner as f64) * r.fraction()) as u32;
                if fw > 0 {
                    let tone = if r.phase == Phase::Paused { Tok::Warning } else { Tok::PrimaryInk };
                    draw::round_rect(
                        &mut s,
                        Rect::new(bar.x, bar.y, fw.max(bar.h), bar.h),
                        bar.h as f32 / 2.0,
                        t.colour(tone),
                    );
                }
            }
            let mut m = node(ax::METER, &r.title, 0, bar, &alloc::format!("copy-progress-{job}"));
            m.value = alloc::format!("{:.0} %", r.fraction() * 100.0);
            self.win.nodes.push(m);
            y += t.px(16) as i32;
            let words = r.words();
            self.win.say((pad, y), TextStyle::Small, &words, Tok::Text2);
            let (ww, wh) = self.win.measure(TextStyle::Small, &words);
            let mut n = node(ax::TEXT, &words, 0, Rect::new(pad, y, ww, wh), &alloc::format!("copy-words-{job}"));
            n.value = words.clone();
            self.win.nodes.push(n);
            y += t.px(28) as i32;
            match &r.phase {
                Phase::Running | Phase::Paused => {
                    let paused = r.phase == Phase::Paused;
                    let keys = [
                        (
                            if paused { "Resume" } else { "Pause" },
                            false,
                            Hit::Pause(i),
                            alloc::format!("copy-pause-{job}"),
                        ),
                        ("Cancel", false, Hit::Cancel(i), alloc::format!("copy-cancel-{job}")),
                        (
                            if r.details { "Fewer details" } else { "More details" },
                            false,
                            Hit::Details(i),
                            alloc::format!("copy-details-{job}"),
                        ),
                    ];
                    self.keys(pad, y, &keys);
                    y += t.px(44) as i32;
                    if r.details {
                        y = self.graph(&r, pad, y, inner);
                    }
                }
                Phase::Conflict(c) => {
                    let ask = alloc::format!("The destination already has a file named \u{201C}{}\u{201D}.", c.name());
                    self.win.say((pad, y), TextStyle::Body, &ask, Tok::Text);
                    let (aw, ah) = self.win.measure(TextStyle::Body, &ask);
                    self.win.nodes.push(node(ax::TEXT, &ask, 0, Rect::new(pad, y, aw, ah), "copy-conflict"));
                    y += t.px(30) as i32;
                    let keys = [
                        ("Replace the file in the destination", true, Hit::Replace(i), String::from("copy-replace")),
                        ("Skip this file", false, Hit::Skip(i), String::from("copy-skip")),
                    ];
                    self.keys(pad, y, &keys);
                    y += t.px(40) as i32;
                    self.keys(
                        pad,
                        y,
                        &[("Let me decide for each file", false, Hit::Decide(i), String::from("copy-decide"))],
                    );
                    y += t.px(40) as i32;
                    self.checkbox(
                        Rect::new(pad, y, inner, t.px(24)),
                        r.all,
                        Hit::All(i),
                        "Do this for all conflicts",
                        "copy-all",
                    );
                    y += t.px(34) as i32;
                }
                Phase::Deciding(clashes) => {
                    self.win.say((pad, y), TextStyle::Body, "Which files do you want to keep?", Tok::Text);
                    y += t.px(26) as i32;
                    let half = (inner / 2) as i32;
                    for (k, c) in clashes.iter().enumerate() {
                        self.win.say((pad, y), TextStyle::Body, c.name(), Tok::Text);
                        y += t.px(22) as i32;
                        let new = alloc::format!("New: {}, {}", size(c.src_size), when(c.src_modified_ns));
                        let there = alloc::format!("There: {}, {}", size(c.dst_size), when(c.dst_modified_ns));
                        let row = t.px(26);
                        self.checkbox(
                            Rect::new(pad, y, half as u32 - t.px(8), row),
                            c.keep_src,
                            Hit::KeepSrc(i, k),
                            &new,
                            &alloc::format!("copy-keep-new-{}", c.name()),
                        );
                        self.checkbox(
                            Rect::new(pad + half, y, half as u32, row),
                            c.keep_dst,
                            Hit::KeepDst(i, k),
                            &there,
                            &alloc::format!("copy-keep-there-{}", c.name()),
                        );
                        y += t.px(32) as i32;
                    }
                    self.keys(
                        pad,
                        y,
                        &[
                            ("Continue", true, Hit::Continue(i), String::from("copy-continue")),
                            ("Cancel", false, Hit::Cancel(i), alloc::format!("copy-cancel-{job}")),
                        ],
                    );
                    y += t.px(44) as i32;
                }
                Phase::Failed(words) => {
                    let said = alloc::format!("It couldn't go on: {words}");
                    self.win.say((pad, y), TextStyle::Body, &said, Tok::Error);
                    let (aw, ah) = self.win.measure(TextStyle::Body, &said);
                    let mut n = node(ax::TEXT, &said, 0, Rect::new(pad, y, aw.min(inner), ah), "copy-failed");
                    n.value = said.clone();
                    self.win.nodes.push(n);
                    y += t.px(30) as i32;
                    let keys = [
                        ("Try again", true, Hit::TryAgain(i), String::from("copy-try-again")),
                        ("Skip", false, Hit::SkipFailed(i), String::from("copy-skip-failed")),
                        ("Cancel", false, Hit::Cancel(i), alloc::format!("copy-cancel-{job}")),
                    ];
                    self.keys(pad, y, &keys);
                    y += t.px(44) as i32;
                }
                Phase::Done | Phase::Cancelled => {}
            }
            let block = Rect::new(pad, top, inner, (y - top) as u32);
            let mut g = node(ax::GROUP, &r.title, 0, block, &alloc::format!("copy-run-{job}"));
            g.value = String::from(match r.phase {
                Phase::Running => "running",
                Phase::Paused => "paused",
                Phase::Conflict(_) => "conflict",
                Phase::Deciding(_) => "deciding",
                Phase::Failed(_) => "failed",
                Phase::Done => "done",
                Phase::Cancelled => "cancelled",
            });
            self.win.nodes.push(g);
            fill(&mut self.win.surface(), Rect::new(pad, y, inner, 1), t.colour(Tok::Border).xrgb());
            y += t.px(14) as i32;
            if y > h as i32 {
                break;
            }
        }
        self.focus = self.focus.min(self.hits.len().saturating_sub(1));
        self.win.present();
    }

    /// The speed over the last minute, in the fleet's pen; its figure.
    fn graph(&mut self, r: &Run, x: i32, y: i32, w: u32) -> i32 {
        let t = self.win.theme;
        let gh = t.px(90);
        let g = Rect::new(x, y, w, gh);
        let speeds = r.speeds();
        let top = speeds.iter().copied().fold(1.0, f64::max);
        {
            let mut s = self.win.surface();
            let mut faint = t.colour(Tok::Border);
            faint.a = 160;
            fui::plot::grid(&mut s, g, 3, 6, faint);
            if speeds.len() >= 2 {
                let step = w as f64 / (speeds.len() - 1) as f64;
                let pts: Vec<fui::plot::Pt> = speeds
                    .iter()
                    .enumerate()
                    .map(|(k, v)| fui::plot::Pt {
                        x: x as f64 + k as f64 * step,
                        y: y as f64 + gh as f64 * (1.0 - v / top),
                    })
                    .collect();
                let accent = t.colour(Tok::PrimaryInk);
                fui::plot::fill_under(&mut s, &pts, y as f64, (y + gh as i32) as f64, accent, fui::plot::Ink::Body);
                fui::plot::stroke(&mut s, &pts, 1.5 * t.scale as f64, accent, t.mode == Mode::Dark);
            }
        }
        let words = r.speed_words();
        let mut n = node(ax::IMG, "Speed", 0, g, &alloc::format!("copy-graph-{}", r.job));
        n.value = alloc::format!("{} samples, {}", speeds.len(), words);
        self.win.nodes.push(n);
        let mut y = y + gh as i32 + t.px(8) as i32;
        self.win.say((x, y), TextStyle::Small, &words, Tok::Text2);
        y += t.px(20) as i32;
        let name = alloc::format!("Name: {}", r.current.rsplit('/').next().unwrap_or(""));
        self.win.say((x, y), TextStyle::Small, &name, Tok::Text2);
        y + t.px(26) as i32
    }

    fn press(&mut self, h: Hit) {
        let job_of = |c: &Copies, i: usize| c.lives.get(i).map(|l| l.run.job);
        match h {
            Hit::Pause(i) => {
                if let Some(l) = self.lives.get_mut(i) {
                    l.run.phase = if l.run.phase == Phase::Paused { Phase::Running } else { Phase::Paused };
                    fstd::info!(
                        "files: copy {} {}",
                        l.run.job,
                        if l.run.phase == Phase::Paused { "paused" } else { "resumed" }
                    );
                }
            }
            Hit::Cancel(i) => {
                if let Some(job) = job_of(self, i) {
                    let _ = fstd::fs::copy_cancel(job);
                    fstd::info!("files: copy {job} cancelled");
                    self.lives[i].run.phase = Phase::Cancelled;
                }
            }
            Hit::Details(i) => {
                if let Some(l) = self.lives.get_mut(i) {
                    l.run.details = !l.run.details;
                }
            }
            Hit::All(i) => {
                if let Some(l) = self.lives.get_mut(i) {
                    l.run.all = !l.run.all;
                }
            }
            Hit::Replace(i) | Hit::Skip(i) => {
                if let Some(l) = self.lives.get_mut(i) {
                    let c = if matches!(h, Hit::Replace(_)) { v::CHOOSE_REPLACE } else { v::CHOOSE_SKIP };
                    match fstd::fs::copy_decide(l.run.job, c, l.run.all) {
                        Ok(()) => l.run.phase = Phase::Running,
                        Err(e) => l.run.phase = Phase::Failed(e.describe()),
                    }
                }
            }
            Hit::Decide(i) => {
                if let Some(l) = self.lives.get_mut(i) {
                    match fstd::fs::copy_conflicts(l.run.job) {
                        Ok(all) => l.run.phase = Phase::Deciding(all.iter().map(Clash::from_wire).collect()),
                        Err(e) => l.run.phase = Phase::Failed(e.describe()),
                    }
                }
            }
            Hit::KeepSrc(i, k) | Hit::KeepDst(i, k) => {
                if let Some(l) = self.lives.get_mut(i)
                    && let Phase::Deciding(clashes) = &mut l.run.phase
                    && let Some(c) = clashes.get_mut(k)
                {
                    if matches!(h, Hit::KeepSrc(..)) {
                        c.keep_src = !c.keep_src;
                    } else {
                        c.keep_dst = !c.keep_dst;
                    }
                }
            }
            Hit::Continue(i) => {
                if let Some(l) = self.lives.get_mut(i)
                    && let Phase::Deciding(clashes) = &l.run.phase
                {
                    let choices: Vec<(String, u8)> = clashes.iter().map(|c| (c.src.clone(), c.choice())).collect();
                    match fstd::fs::copy_each(l.run.job, choices) {
                        Ok(()) => l.run.phase = Phase::Running,
                        Err(e) => l.run.phase = Phase::Failed(e.describe()),
                    }
                }
            }
            Hit::TryAgain(i) | Hit::SkipFailed(i) => {
                if let Some(l) = self.lives.get_mut(i) {
                    let c = if matches!(h, Hit::TryAgain(_)) { v::CHOOSE_TRY_AGAIN } else { v::CHOOSE_SKIP };
                    match fstd::fs::copy_decide(l.run.job, c, false) {
                        Ok(()) => l.run.phase = Phase::Running,
                        Err(e) => l.run.phase = Phase::Failed(e.describe()),
                    }
                }
            }
        }
    }

    /// One event of the copy window.
    pub fn event(&mut self, ordinal: u32, p: &[u8]) {
        match ordinal {
            st::EV_POINTER => {
                let Ok(e) = fwire::from_payload::<st::Pointer>(p) else { return };
                let was = (self.hovered, self.pressed);
                self.hovered = if e.kind == st::PointerKind::Leave {
                    None
                } else {
                    self.hits.iter().rev().find(|(r, _)| inside(*r, e.x, e.y)).map(|(_, h)| *h)
                };
                match e.kind {
                    st::PointerKind::Down if e.buttons & 1 != 0 => self.pressed = self.hovered,
                    st::PointerKind::Up => {
                        if let Some(h) = self.pressed.take().filter(|h| Some(*h) == self.hovered) {
                            if let Some(k) = self.hits.iter().position(|(_, x)| *x == h) {
                                self.focus = k;
                            }
                            self.press(h);
                            self.draw();
                            return;
                        }
                    }
                    _ => {}
                }
                if (self.hovered, self.pressed) != was {
                    self.draw();
                }
            }
            st::EV_KEY => {
                let Ok(k) = fwire::from_payload::<st::Key>(p) else { return };
                if !k.down || self.hits.is_empty() {
                    return;
                }
                let n = self.hits.len();
                match k.usage {
                    app_files::key::TAB if k.modifiers & app_files::key::SHIFT != 0 => {
                        self.focus = (self.focus + n - 1) % n
                    }
                    app_files::key::TAB => self.focus = (self.focus + 1) % n,
                    app_files::key::ENTER | app_files::key::SPACE => {
                        let h = self.hits[self.focus.min(n - 1)].1;
                        self.press(h);
                    }
                    _ => return,
                }
                self.draw();
            }
            st::EV_THEME => self.draw(),
            st::EV_SCALE => {
                if self.win.reopen().is_ok() {
                    self.draw();
                }
            }
            _ => {}
        }
    }
}

/// "12:05, 19 Nov 2026".
fn when(ns: u64) -> String {
    if ns == 0 {
        return String::from("no date");
    }
    let secs = ns / 1_000_000_000;
    let (y, m, d) = app_session::import::date(secs);
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let day = secs % 86_400;
    alloc::format!("{:02}:{:02}, {d} {} {y}", day / 3600, day / 60 % 60, MONTHS[(m as usize + 11) % 12])
}
