// SPDX-License-Identifier: Apache-2.0
//! Welcome (TASK-102, PRD § 8): on a disk with no accounts, makes the first
//! one, the Owner, then logs it in. Ends once an account exists and a
//! session has started (made here, or anywhere else).
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use app_session::form::{self, FIELDS, Form, Outcome};
use app_session::key;
use app_session::screen::{Look, Screen, inside};
use fcore_abi::{FErr, signal};
use fstd::ipc::{CallError, Client};
use fstd::time::{Duration, Instant};
use fui::layout::Rect;
use fui::theme::{TextStyle, Tok};
use fwire::proto::accounts as a;
use fwire::proto::conductor as c;
use fwire::proto::stage as st;

fstd::program!(run);

struct Welcome {
    screen: Screen,
    accounts: Client,
    form: Form,
    /// "Freally OS started in 1.2 seconds."
    started: String,
    /// What accounts said, when it refused.
    problem: Option<String>,
    /// Making the account (the meter shows).
    busy: bool,
    wells: [Rect; FIELDS],
    create: Rect,
    look: Look,
}

fn call<Req: fwire::Wire, Rep: fwire::Wire>(acc: &mut Client, ordinal: u32, req: &Req) -> Result<Rep, CallError> {
    acc.call::<_, Rep>(ordinal, req, Vec::new(), Instant::after(Duration::from_secs(30))).map(|(r, _)| r)
}

impl Welcome {
    fn draw(&mut self) {
        let s = &mut self.screen;
        let t = s.theme;
        s.ground(false);
        let pad = t.px(32) as i32;
        let (_, title, _) = t.text(TextStyle::Title);
        let (_, body, _) = t.text(TextStyle::Body);
        let key_h = t.px(40);
        let inner = (title + t.px(8) + body + t.px(24) + FIELDS as u32 * s.field_height() + key_h + t.px(32)) as i32;
        let m = s.module(560, ((inner + 2 * pad) as f32 / t.scale) as u32);
        let (x, w) = (m.x + pad, m.w - 2 * pad as u32);
        let mut y = m.y + pad;
        s.say((x, y), TextStyle::Title, &self.started, Tok::Text);
        y += (title + t.px(8)) as i32;
        s.say((x, y), TextStyle::Body, "Make the account that owns this computer.", Tok::Text2);
        y += (body + t.px(24)) as i32;
        for i in 0..FIELDS {
            let f = &self.form;
            let secret = i >= form::PASSWORD;
            let focused = f.focus == i && !self.busy;
            self.wells[i] =
                s.field((x, y, w), form::LABELS[i], &f.fields[i], secret, focused, f.errors[i], form::IDS[i]);
            y += s.field_height() as i32;
        }
        if let Some(p) = &self.problem {
            s.say((x, y), TextStyle::Small, p, Tok::Error);
        }
        y += t.px(32) as i32 - t.px(8) as i32;
        if self.busy {
            s.meter(Rect::new(x, y + t.px(16) as i32, w, key_h), "Making your account…", "welcome-meter");
        } else {
            let kw = t.px(180);
            self.create = Rect::new(x + w as i32 - kw as i32, y, kw, key_h);
            s.key(self.create, "Create account", true, self.look, "welcome-create");
        }
        s.present();
    }

    /// Makes the account, then logs it in. `true` when there's a session.
    fn create(&mut self) -> bool {
        self.busy = true;
        self.problem = None;
        self.draw();
        let f = &self.form;
        let req = a::Create {
            username: f.fields[form::USERNAME].clone(),
            display_name: String::from(f.fields[form::NAME].trim()),
            password: f.fields[form::PASSWORD].clone(),
            owner_password: String::new(),
        };
        let mut login = a::Login { username: req.username.clone(), password: req.password.clone() };
        let mut req = req;
        let made = call::<_, ()>(&mut self.accounts, a::CREATE, &req);
        fstd::secret::wipe_str(&mut req.password);
        let done = made.and_then(|_| call::<_, ()>(&mut self.accounts, a::LOGIN, &login));
        fstd::secret::wipe_str(&mut login.password);
        self.busy = false;
        match done {
            Ok(()) => {
                fstd::info!("welcome: made {} (the Owner) and logged in", login.username);
                self.form.forget_passwords();
                true
            }
            Err(CallError::Remote(code, text)) => {
                if code == FErr::AlreadyExists as i32 {
                    self.form.errors[form::USERNAME] = Some("That username is taken.");
                    self.form.focus = form::USERNAME;
                } else {
                    self.problem = Some(text);
                }
                false
            }
            Err(e) => {
                self.problem = Some(alloc::format!("accounts didn't answer ({e:?})."));
                false
            }
        }
    }

    fn act(&mut self, o: Outcome) -> bool {
        match o {
            Outcome::Changed => {
                self.problem = None;
                self.draw();
                false
            }
            Outcome::Create => {
                let made = self.create();
                if !made {
                    self.draw();
                }
                made
            }
            Outcome::Nothing => false,
        }
    }

    /// One event: `true` once there's a session.
    fn event(&mut self, ordinal: u32, p: &[u8]) -> bool {
        match ordinal {
            st::EV_KEY => {
                let Ok(k) = fwire::from_payload::<st::Key>(p) else { return false };
                if !k.down || self.busy {
                    return false;
                }
                let o = self.form.key(k.usage, k.ch, k.modifiers & key::SHIFT != 0);
                self.act(o)
            }
            st::EV_POINTER => {
                let Ok(e) = fwire::from_payload::<st::Pointer>(p) else { return false };
                let over = inside(self.create, e.x, e.y);
                let was = self.look;
                self.look.hovered = over && e.kind != st::PointerKind::Leave;
                match e.kind {
                    st::PointerKind::Down if e.buttons & 1 != 0 => {
                        if let Some(i) = self.wells.iter().position(|w| inside(*w, e.x, e.y)) {
                            let o = self.form.focus_on(i);
                            return self.act(o);
                        }
                        self.look.pressed = over;
                    }
                    st::PointerKind::Up => {
                        let pressed = core::mem::take(&mut self.look.pressed);
                        if pressed && over {
                            let o = self.form.submit();
                            return self.act(o);
                        }
                    }
                    _ => {}
                }
                if self.look != was {
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
}

/// Whether Welcome is needed: no account yet, no session.
fn needed(acc: &mut Client) -> bool {
    let none = call::<_, a::Users>(acc, a::LIST, &()).is_ok_and(|u| u.users.is_empty());
    let session = call::<_, a::Who>(acc, a::SESSION, &()).is_ok_and(|w| !w.user.username.is_empty());
    none && !session
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    let mut accounts = match fstd::service::connect_within("accounts", Duration::from_secs(30)) {
        Ok(ch) => Client::new(ch),
        Err(e) => {
            fstd::error!("welcome: accounts isn't here ({e:?})");
            return 1;
        }
    };
    // Not needed (an account was made meanwhile): no window at all.
    if !needed(&mut accounts) {
        return 0;
    }
    let screen = match Screen::open("Welcome") {
        Ok(s) => s,
        Err(e) => {
            fstd::error!("welcome: {e}");
            return 1;
        }
    };
    let started = fstd::service::ask::<_, c::BootTimeline>(c::BOOT_TIMELINE, &())
        .ok()
        .and_then(|t| form::started_in(&t.stages))
        .map_or_else(
            || String::from("Welcome to Freally OS."),
            |s| alloc::format!("Welcome. Freally OS started in {s:.1} seconds."),
        );
    let mut w = Welcome {
        screen,
        accounts,
        form: Form::new(),
        started,
        problem: None,
        busy: false,
        wells: [Rect::new(0, 0, 0, 0); FIELDS],
        create: Rect::new(0, 0, 0, 0),
        look: Look::default(),
    };
    w.draw();
    fstd::info!("welcome: shown");
    let mut next_look = Instant::now().as_ns();
    loop {
        let _ =
            w.screen.events.0.wait(signal::READABLE | signal::PEER_CLOSED, Instant::after(Duration::from_millis(500)));
        let Ok(events) = w.screen.events() else { return 0 };
        for (ordinal, p) in events {
            if w.event(ordinal, &p) {
                return 0;
            }
        }
        // An account made elsewhere (the test door), or a session started.
        if Instant::now().as_ns() >= next_look {
            next_look = Instant::now().as_ns() + 500_000_000;
            if !needed(&mut w.accounts) {
                fstd::info!("welcome: no longer needed");
                return 0;
            }
        }
    }
}
