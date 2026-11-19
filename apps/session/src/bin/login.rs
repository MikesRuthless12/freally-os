// SPDX-License-Identifier: Apache-2.0
//! Login (TASK-102, PRD § 8): the wallpaper dimmed 40 %, the accounts by
//! display name, the password well and *Log in*; *Shut down* and *Restart*
//! at the bottom right. Ends once a session has started.
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use app_session::key;
use app_session::login::{Focus, Login, Outcome, User};
use fapp::{Look, Window, inside};
use fcore_abi::{FErr, signal};
use fstd::ipc::{CallError, Client};
use fstd::time::{Duration, Instant};
use fui::draw;
use fui::layout::Rect;
use fui::theme::{TextStyle, Tok};
use fwire::proto::accounts as a;
use fwire::proto::ax;
use fwire::proto::conductor as c;
use fwire::proto::stage as st;

fstd::program!(run);

/// The keys, by where they are in `rects`.
const LOG_IN: usize = 0;
const SHUT_DOWN: usize = 1;
const RESTART: usize = 2;
const IDS: [&str; 3] = ["login-submit", "login-shutdown", "login-restart"];
const LABELS: [&str; 3] = ["Log in", "Shut down", "Restart"];

struct Screen2 {
    screen: Window,
    accounts: Client,
    login: Login,
    busy: bool,
    rows: Vec<Rect>,
    well: Rect,
    keys: [Rect; 3],
    looks: [Look; 3],
}

fn call<Req: fwire::Wire, Rep: fwire::Wire>(acc: &mut Client, ordinal: u32, req: &Req) -> Result<Rep, CallError> {
    acc.call::<_, Rep>(ordinal, req, Vec::new(), Instant::after(Duration::from_secs(30))).map(|(r, _)| r)
}

fn users(acc: &mut Client) -> Vec<User> {
    call::<_, a::Users>(acc, a::LIST, &())
        .map(|u| u.users.into_iter().map(|u| User { username: u.username, display_name: u.display_name }).collect())
        .unwrap_or_default()
}

impl Screen2 {
    fn draw(&mut self) {
        let s = &mut self.screen;
        let t = s.theme;
        s.ground(true);
        let pad = t.px(32) as i32;
        let row = t.px(52);
        let (_, title, _) = t.text(TextStyle::Title);
        let key_h = t.px(40);
        let n = self.login.users.len() as u32;
        let inner = title + t.px(20) + n * row + t.px(16) + s.field_height() + key_h;
        let m = s.module(480, ((inner as i32 + 2 * pad) as f32 / t.scale) as u32);
        let (x, w) = (m.x + pad, m.w - 2 * pad as u32);
        let mut y = m.y + pad;
        s.say((x, y), TextStyle::Title, "Log in", Tok::Text);
        y += (title + t.px(20)) as i32;
        // The accounts: the chosen one raised, a green thread at its edge.
        let list = Rect::new(x, y, w, n * row);
        self.rows.clear();
        let mut nodes = Vec::new();
        for (i, u) in self.login.users.iter().enumerate() {
            let r = Rect::new(x, y + i as i32 * row as i32, w, row);
            let chosen = i == self.login.chosen;
            if chosen {
                let mut sf = s.surface();
                draw::round_rect(&mut sf, r, 6.0 * t.scale, t.colour(Tok::Surface2));
                fui::draw::fill(
                    &mut sf,
                    Rect::new(r.x, r.y + t.px(8) as i32, t.px(2).max(1), row - t.px(16)),
                    t.colour(Tok::PrimaryInk).xrgb(),
                );
            }
            let tx = r.x + t.px(16) as i32;
            s.say((tx, r.y + t.px(6) as i32), TextStyle::Body, u.shown(), Tok::Text);
            s.say((tx, r.y + t.px(28) as i32), TextStyle::Small, &u.username, Tok::Text2);
            let mut o =
                fstd::ax::node(1, ax::OPTION, u.shown(), if chosen { ax::SELECTED } else { 0 }, (r.x, r.y, r.w, r.h));
            o.testid = alloc::format!("login-user-{}", u.username);
            nodes.push(o);
            self.rows.push(r);
        }
        let mut l = fstd::ax::node(ax::ROOT, ax::LISTBOX, "Accounts", 0, (list.x, list.y, list.w, list.h));
        l.testid = String::from("login-users");
        s.nodes.push(l);
        s.nodes.extend(nodes);
        y += (n * row + t.px(16)) as i32;
        let focused = self.login.focus == Focus::Password && !self.busy;
        self.well =
            s.field((x, y, w), "Password", &self.login.password, true, focused, self.login.error, "login-password");
        y += s.field_height() as i32;
        if self.busy {
            s.meter(Rect::new(x, y + t.px(16) as i32, w, key_h), "Checking your password…", "login-meter");
        } else {
            let kw = t.px(140);
            self.keys[LOG_IN] = Rect::new(x + w as i32 - kw as i32, y, kw, key_h);
        }
        // Shut down and Restart, bottom right of the screen.
        let (kw, gap, edge) = (t.px(128), t.px(12) as i32, t.px(24) as i32);
        let ky = s.h as i32 - edge - key_h as i32;
        self.keys[RESTART] = Rect::new(s.w as i32 - edge - kw as i32, ky, kw, key_h);
        self.keys[SHUT_DOWN] = Rect::new(self.keys[RESTART].x - gap - kw as i32, ky, kw, key_h);
        for k in 0..3 {
            if k == LOG_IN && self.busy {
                continue;
            }
            let mut look = self.looks[k];
            look.focused = match k {
                SHUT_DOWN => self.login.focus == Focus::ShutDown,
                RESTART => self.login.focus == Focus::Restart,
                _ => false,
            };
            s.key(self.keys[k], LABELS[k], k == LOG_IN, look, IDS[k]);
        }
        s.present();
    }

    /// Logs the chosen account in. `true` when there's a session.
    fn submit(&mut self) -> bool {
        let Some(user) = self.login.user().map(|u| u.username.clone()) else { return false };
        self.busy = true;
        self.draw();
        let mut req = a::Login { username: user.clone(), password: self.login.password.clone() };
        self.login.forget_password();
        let done = call::<_, ()>(&mut self.accounts, a::LOGIN, &req);
        fstd::secret::wipe_str(&mut req.password);
        self.busy = false;
        match done {
            Ok(()) => {
                fstd::info!("login: {user} logged in");
                true
            }
            Err(CallError::Remote(code, _)) => {
                self.login.refused(code == FErr::Busy as i32);
                self.draw();
                false
            }
            Err(e) => {
                fstd::error!("login: accounts didn't answer ({e:?})");
                self.login.refused(false);
                self.draw();
                false
            }
        }
    }

    fn power(&mut self, off: bool) {
        fstd::info!("login: {}", if off { "Shut down" } else { "Restart" });
        if let Err(e) = fstd::service::ask::<_, ()>(c::POWER, &c::Power { off }) {
            fstd::error!("login: conductor refused {} ({e:?})", if off { "Shut down" } else { "Restart" });
        }
    }

    fn act(&mut self, o: Outcome) -> bool {
        match o {
            Outcome::Changed => {
                self.draw();
                false
            }
            Outcome::Submit => self.submit(),
            Outcome::ShutDown => {
                self.power(true);
                false
            }
            Outcome::Restart => {
                self.power(false);
                false
            }
            Outcome::Nothing => false,
        }
    }

    fn event(&mut self, ordinal: u32, p: &[u8]) -> bool {
        match ordinal {
            st::EV_KEY => {
                let Ok(k) = fwire::from_payload::<st::Key>(p) else { return false };
                if !k.down || self.busy {
                    return false;
                }
                let o = self.login.key(k.usage, k.ch, k.modifiers & key::SHIFT != 0);
                self.act(o)
            }
            st::EV_POINTER => {
                let Ok(e) = fwire::from_payload::<st::Pointer>(p) else { return false };
                let gone = e.kind == st::PointerKind::Leave;
                let was = self.looks;
                for k in 0..3 {
                    self.looks[k].hovered = !gone && inside(self.keys[k], e.x, e.y);
                }
                match e.kind {
                    st::PointerKind::Down if e.buttons & 1 != 0 => {
                        if let Some(i) = self.rows.iter().position(|r| inside(*r, e.x, e.y)) {
                            let o = self.login.choose(i);
                            return self.act(o);
                        }
                        if inside(self.well, e.x, e.y) && self.login.focus != Focus::Password {
                            self.login.focus = Focus::Password;
                            return self.act(Outcome::Changed);
                        }
                        for l in self.looks.iter_mut() {
                            l.pressed = l.hovered;
                        }
                    }
                    st::PointerKind::Up => {
                        let pressed = (0..3).find(|&k| self.looks[k].pressed);
                        for l in self.looks.iter_mut() {
                            l.pressed = false;
                        }
                        if let Some(k) = pressed.filter(|&k| self.looks[k].hovered) {
                            let o = match k {
                                LOG_IN if !self.login.password.is_empty() => Outcome::Submit,
                                LOG_IN => Outcome::Nothing,
                                SHUT_DOWN => Outcome::ShutDown,
                                _ => Outcome::Restart,
                            };
                            return self.act(o);
                        }
                    }
                    _ => {}
                }
                if self.looks != was {
                    self.draw();
                }
                false
            }
            st::EV_THEME | st::EV_SCALE => {
                self.draw();
                false
            }
            _ => false,
        }
    }

    /// A session started (here, or anywhere else).
    fn session(&mut self) -> bool {
        call::<_, a::Who>(&mut self.accounts, a::SESSION, &()).is_ok_and(|w| !w.user.username.is_empty())
    }
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    let mut accounts = match fstd::service::connect_within("accounts", Duration::from_secs(30)) {
        Ok(ch) => Client::new(ch),
        Err(e) => {
            fstd::error!("login: accounts isn't here ({e:?})");
            return 1;
        }
    };
    let found = users(&mut accounts);
    if found.is_empty() {
        fstd::info!("login: no accounts: the welcome screen makes the first");
        return 0;
    }
    // Someone is logged in already: no window at all.
    if call::<_, a::Who>(&mut accounts, a::SESSION, &()).is_ok_and(|w| !w.user.username.is_empty()) {
        return 0;
    }
    let screen = match Window::open("Log in", fwire::proto::stage::Kind::Lock, (0, 0)) {
        Ok(s) => s,
        Err(e) => {
            fstd::error!("login: {e}");
            return 1;
        }
    };
    let mut l = Screen2 {
        screen,
        accounts,
        login: Login::new(found),
        busy: false,
        rows: Vec::new(),
        well: Rect::new(0, 0, 0, 0),
        keys: [Rect::new(0, 0, 0, 0); 3],
        looks: [Look::default(); 3],
    };
    l.draw();
    fstd::info!("login: shown ({} account(s))", l.login.users.len());
    let mut next_look = Instant::now().as_ns();
    loop {
        let _ =
            l.screen.events.0.wait(signal::READABLE | signal::PEER_CLOSED, Instant::after(Duration::from_millis(500)));
        let Ok(events) = l.screen.events() else { return 0 };
        for (ordinal, p) in events {
            if l.event(ordinal, &p) {
                return 0;
            }
        }
        if Instant::now().as_ns() >= next_look {
            next_look = Instant::now().as_ns() + 500_000_000;
            if l.session() {
                fstd::info!("login: a session started");
                return 0;
            }
            // An account added meanwhile (the test door).
            let now = users(&mut l.accounts);
            if now.len() != l.login.users.len() {
                let chosen = l.login.chosen.min(now.len().saturating_sub(1));
                l.login.users = now;
                l.login.chosen = chosen;
                l.draw();
            }
        }
    }
}
