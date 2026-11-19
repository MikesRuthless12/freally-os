// SPDX-License-Identifier: Apache-2.0
//! Welcome (TASK-102, PRD § 8): on a disk with no accounts, makes the first
//! one, the Owner, then logs it in; or (TASK-103) *Import an account…*: an
//! account file chosen, its password, its preview, *Import*. Ends once an
//! account exists and a session has started (made here, or anywhere else).
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use app_session::form::{self, FIELDS, Form, Outcome};
use app_session::import::{self, Import};
use app_session::key;
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

/// The keys, by where they are in `keys` and `looks`.
const CREATE: usize = 0;
const IMPORT: usize = 1;
const BACK: usize = 2;
const GO: usize = 3;

struct Welcome {
    screen: Window,
    accounts: Client,
    form: Form,
    /// The import dialog, while it's open.
    import: Option<Import>,
    /// "Freally OS started in 1.2 seconds."
    started: String,
    /// What accounts said, when it refused.
    problem: Option<String>,
    /// What the meter says while accounts works (none: no meter).
    busy: Option<&'static str>,
    wells: [Rect; FIELDS],
    rows: Vec<Rect>,
    keys: [Rect; 4],
    looks: [Look; 4],
}

fn call<Req: fwire::Wire, Rep: fwire::Wire>(acc: &mut Client, ordinal: u32, req: &Req) -> Result<Rep, CallError> {
    acc.call::<_, Rep>(ordinal, req, Vec::new(), Instant::after(Duration::from_secs(120))).map(|(r, _)| r)
}

fn words(e: CallError) -> String {
    match e {
        CallError::Remote(_, text) => text,
        e => alloc::format!("accounts didn't answer ({e:?})."),
    }
}

impl Welcome {
    fn draw(&mut self) {
        if self.import.is_some() {
            self.draw_import();
        } else {
            self.draw_form();
        }
    }

    fn draw_form(&mut self) {
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
            let focused = f.focus == i && self.busy.is_none();
            self.wells[i] =
                s.field((x, y, w), form::LABELS[i], &f.fields[i], secret, focused, f.errors[i], form::IDS[i]);
            y += s.field_height() as i32;
        }
        if let Some(p) = &self.problem {
            s.say((x, y), TextStyle::Small, p, Tok::Error);
        }
        y += t.px(32) as i32 - t.px(8) as i32;
        if let Some(what) = self.busy {
            s.meter(Rect::new(x, y + t.px(16) as i32, w, key_h), what, "welcome-meter");
        } else {
            let kw = t.px(180);
            self.keys[CREATE] = Rect::new(x + w as i32 - kw as i32, y, kw, key_h);
            self.keys[IMPORT] = Rect::new(x, y, t.px(200), key_h);
            s.key(self.keys[IMPORT], "Import an account…", false, self.looks[IMPORT], "welcome-import");
            s.key(self.keys[CREATE], "Create account", true, self.looks[CREATE], "welcome-create");
        }
        s.present();
    }

    fn draw_import(&mut self) {
        let Some(im) = self.import.as_ref() else { return };
        let s = &mut self.screen;
        let t = s.theme;
        s.ground(false);
        let pad = t.px(32) as i32;
        let row = t.px(40);
        let (_, title, _) = t.text(TextStyle::Title);
        let (_, body, _) = t.text(TextStyle::Body);
        let key_h = t.px(40);
        let files = im.files.len().max(1) as u32;
        let inner =
            title + t.px(8) + body + t.px(16) + files * row + t.px(16) + s.field_height() + body + t.px(16) + key_h;
        let m = s.module(560, ((inner as i32 + 2 * pad) as f32 / t.scale) as u32);
        let (x, w) = (m.x + pad, m.w - 2 * pad as u32);
        let mut y = m.y + pad;
        s.say((x, y), TextStyle::Title, "Import an account", Tok::Text);
        y += (title + t.px(8)) as i32;
        let intro = if im.files.is_empty() {
            "No account files here yet: a file exported on another Freally OS computer goes in /system/imports."
        } else {
            "Choose the account file, then type its password."
        };
        s.say((x, y), TextStyle::Body, intro, Tok::Text2);
        y += (body + t.px(16)) as i32;
        // The files: the chosen one raised, a green thread at its edge.
        self.rows.clear();
        let list = Rect::new(x, y, w, im.files.len() as u32 * row);
        let mut options = Vec::new();
        for (i, name) in im.files.iter().enumerate() {
            let r = Rect::new(x, y + i as i32 * row as i32, w, row);
            let chosen = i == im.chosen;
            if chosen {
                let mut sf = s.surface();
                draw::round_rect(&mut sf, r, 6.0 * t.scale, t.colour(Tok::Surface2));
                draw::fill(
                    &mut sf,
                    Rect::new(r.x, r.y + t.px(8) as i32, t.px(2).max(1), row - t.px(16)),
                    t.colour(Tok::PrimaryInk).xrgb(),
                );
            }
            let shown = Import::shown(name);
            s.say((r.x + t.px(16) as i32, r.y + t.px(10) as i32), TextStyle::Body, shown, Tok::Text);
            let mut o =
                fstd::ax::node(1, ax::OPTION, shown, if chosen { ax::SELECTED } else { 0 }, (r.x, r.y, r.w, r.h));
            o.testid = alloc::format!("welcome-file-{shown}");
            options.push(o);
            self.rows.push(r);
        }
        if !im.files.is_empty() {
            let mut l = fstd::ax::node(ax::ROOT, ax::LISTBOX, "Account files", 0, (list.x, list.y, list.w, list.h));
            l.testid = String::from("welcome-files");
            s.nodes.push(l);
            s.nodes.extend(options);
        }
        y += (files * row + t.px(16)) as i32;
        let focused = self.busy.is_none();
        s.field((x, y, w), "Its password", &im.password, true, focused, im.error.as_deref(), "welcome-import-password");
        y += s.field_height() as i32;
        if let Some(p) = &im.preview {
            s.say((x, y), TextStyle::Body, p, Tok::Text);
            let (pw, ph) = s.measure(TextStyle::Body, p);
            let mut n = fstd::ax::node(ax::ROOT, ax::TEXT, p, 0, (x, y, pw, ph));
            n.testid = String::from("welcome-preview");
            s.nodes.push(n);
        }
        y += (body + t.px(16)) as i32;
        if let Some(what) = self.busy {
            s.meter(Rect::new(x, y + t.px(16) as i32, w, key_h), what, "welcome-meter");
        } else {
            let kw = t.px(140);
            self.keys[BACK] = Rect::new(x, y, kw, key_h);
            self.keys[GO] = Rect::new(x + w as i32 - kw as i32, y, kw, key_h);
            s.key(self.keys[BACK], "Back", false, self.looks[BACK], "welcome-back");
            let label = if im.preview.is_some() { "Import" } else { "Check file" };
            s.key(self.keys[GO], label, true, self.looks[GO], "welcome-import-go");
        }
        s.present();
    }

    /// Makes the account, then logs it in. `true` when there's a session.
    fn create(&mut self) -> bool {
        self.busy = Some("Making your account…");
        self.problem = None;
        self.draw();
        let f = &self.form;
        let mut req = a::Create {
            username: f.fields[form::USERNAME].clone(),
            display_name: String::from(f.fields[form::NAME].trim()),
            password: f.fields[form::PASSWORD].clone(),
            owner_password: String::new(),
        };
        let mut login = a::Login { username: req.username.clone(), password: req.password.clone() };
        let made = call::<_, ()>(&mut self.accounts, a::CREATE, &req);
        fstd::secret::wipe_str(&mut req.password);
        let done = made.and_then(|_| call::<_, ()>(&mut self.accounts, a::LOGIN, &login));
        fstd::secret::wipe_str(&mut login.password);
        self.busy = None;
        match done {
            Ok(()) => {
                fstd::info!("welcome: made {} (the Owner) and logged in", login.username);
                self.form.forget_passwords();
                true
            }
            Err(CallError::Remote(code, _)) if code == FErr::AlreadyExists as i32 => {
                self.form.errors[form::USERNAME] = Some("That username is taken.");
                self.form.focus = form::USERNAME;
                false
            }
            Err(e) => {
                self.problem = Some(words(e));
                false
            }
        }
    }

    /// The import dialog, with the files there are.
    fn open_import(&mut self) {
        let files = call::<_, a::Bundles>(&mut self.accounts, a::BUNDLES, &())
            .map(|b| b.names.into_iter().filter(|n| n.starts_with("system:")).collect())
            .unwrap_or_default();
        self.import = Some(Import::new(files));
        fstd::info!("welcome: import an account");
        self.draw();
    }

    /// The chosen file checked whole with the password: its preview.
    fn preview(&mut self) {
        let Some(im) = self.import.as_ref() else { return };
        let Some(name) = im.file().map(String::from) else { return };
        let mut req = a::Open { name, password: im.password.clone() };
        self.busy = Some("Checking the file…");
        self.draw();
        let got = call::<_, a::Preview>(&mut self.accounts, a::PREVIEW, &req);
        fstd::secret::wipe_str(&mut req.password);
        self.busy = None;
        let Some(im) = self.import.as_mut() else { return };
        match got {
            Ok(p) => im.preview = Some(import::preview_words(&p.username, p.files, p.bytes, p.exported_unix)),
            Err(e) => {
                im.error = Some(words(e));
                im.forget_password();
            }
        }
        self.draw();
    }

    /// The chosen file made into the Owner's account, then logged in.
    fn import(&mut self) -> bool {
        let Some(im) = self.import.as_ref() else { return false };
        let Some(name) = im.file().map(String::from) else { return false };
        let mut req =
            a::Import { name, password: im.password.clone(), rename: String::new(), owner_password: String::new() };
        self.busy = Some("Importing the account…");
        self.draw();
        let made = call::<_, a::User>(&mut self.accounts, a::IMPORT, &req);
        let done = made.and_then(|u| {
            let mut login = a::Login { username: u.username.clone(), password: req.password.clone() };
            let r = call::<_, ()>(&mut self.accounts, a::LOGIN, &login);
            fstd::secret::wipe_str(&mut login.password);
            r.map(|_| u.username)
        });
        fstd::secret::wipe_str(&mut req.password);
        self.busy = None;
        let Some(im) = self.import.as_mut() else { return false };
        match done {
            Ok(user) => {
                fstd::info!("welcome: imported {user} (the Owner) and logged in");
                im.forget_password();
                true
            }
            Err(e) => {
                im.error = Some(words(e));
                im.preview = None;
                im.forget_password();
                self.draw();
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

    fn act_import(&mut self, o: import::Outcome) -> bool {
        match o {
            import::Outcome::Changed => {
                self.draw();
                false
            }
            import::Outcome::Preview => {
                self.preview();
                false
            }
            import::Outcome::Import => self.import(),
            import::Outcome::Back => {
                if let Some(mut im) = self.import.take() {
                    im.forget_password();
                }
                self.draw();
                false
            }
            import::Outcome::Nothing => false,
        }
    }

    /// A key clicked: `true` once there's a session.
    fn click(&mut self, k: usize) -> bool {
        match (k, self.import.as_ref()) {
            (CREATE, None) => {
                let o = self.form.submit();
                self.act(o)
            }
            (IMPORT, None) => {
                self.open_import();
                false
            }
            (BACK, Some(_)) => self.act_import(import::Outcome::Back),
            (GO, Some(im)) => {
                let o = match (im.file(), im.password.is_empty(), im.preview.is_some()) {
                    (None, ..) | (_, true, _) => import::Outcome::Nothing,
                    (_, false, true) => import::Outcome::Import,
                    (_, false, false) => import::Outcome::Preview,
                };
                self.act_import(o)
            }
            _ => false,
        }
    }

    /// The keys shown now.
    fn shown_keys(&self) -> [usize; 2] {
        if self.import.is_some() { [BACK, GO] } else { [IMPORT, CREATE] }
    }

    /// One event: `true` once there's a session.
    fn event(&mut self, ordinal: u32, p: &[u8]) -> bool {
        match ordinal {
            st::EV_KEY => {
                let Ok(k) = fwire::from_payload::<st::Key>(p) else { return false };
                if !k.down || self.busy.is_some() {
                    return false;
                }
                if let Some(im) = self.import.as_mut() {
                    let o = im.key(k.usage, k.ch);
                    return self.act_import(o);
                }
                let o = self.form.key(k.usage, k.ch, k.modifiers & key::SHIFT != 0);
                self.act(o)
            }
            st::EV_POINTER => {
                let Ok(e) = fwire::from_payload::<st::Pointer>(p) else { return false };
                if self.busy.is_some() {
                    return false;
                }
                let gone = e.kind == st::PointerKind::Leave;
                let was = self.looks;
                let shown = self.shown_keys();
                for k in 0..4 {
                    self.looks[k].hovered = !gone && shown.contains(&k) && inside(self.keys[k], e.x, e.y);
                }
                match e.kind {
                    st::PointerKind::Down if e.buttons & 1 != 0 => {
                        if let Some(im) = self.import.as_mut() {
                            if let Some(i) = self.rows.iter().position(|r| inside(*r, e.x, e.y)) {
                                let o = im.choose(i);
                                return self.act_import(o);
                            }
                        } else if let Some(i) = self.wells.iter().position(|w| inside(*w, e.x, e.y)) {
                            let o = self.form.focus_on(i);
                            return self.act(o);
                        }
                        for l in self.looks.iter_mut() {
                            l.pressed = l.hovered;
                        }
                    }
                    st::PointerKind::Up => {
                        let pressed = (0..4).find(|&k| self.looks[k].pressed);
                        for l in self.looks.iter_mut() {
                            l.pressed = false;
                        }
                        if let Some(k) = pressed.filter(|&k| self.looks[k].hovered) {
                            return self.click(k);
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
    let screen = match Window::open("Welcome", fwire::proto::stage::Kind::Lock, (0, 0)) {
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
        import: None,
        started,
        problem: None,
        busy: None,
        wells: [Rect::new(0, 0, 0, 0); FIELDS],
        rows: Vec::new(),
        keys: [Rect::new(0, 0, 0, 0); 4],
        looks: [Look::default(); 4],
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
