// SPDX-License-Identifier: Apache-2.0
//! cdp-runner: runs the OS leg's specs over CDP against a VM cdp-host serves.
//!
//! `cdp-runner --endpoint 127.0.0.1:9433 --com1-log <path> --specs boot.title,…
//!  [--failures <dir>]` (started by `cargo xtask cdp --os`, which boots the VM
//! for each setup). Every spec line reads `▶ @id … pass (1.2s)`.
//!
//! - **The guard:** after each spec, any `ERROR` line on COM1 the spec didn't
//!   expect fails it.
//! - **The inventory** (every control and service ordinal touched) arrives with
//!   `cdpd` in Phase 3; Phase 0 has no controls.

mod art;
mod bootcheck;
mod guard;
mod inventory;
mod site;
mod sweep;

#[path = "../../../e2e/os/specs/a11y.rs"]
mod a11y;
#[path = "../../../e2e/os/specs/accounts.rs"]
mod accounts;
#[path = "../../../e2e/os/specs/boot.rs"]
mod boot;
#[path = "../../../e2e/os/specs/calculator.rs"]
mod calculator;
#[path = "../../../e2e/os/specs/cdp.rs"]
mod cdp_specs;
#[path = "../../../e2e/os/specs/crisp.rs"]
mod crisp;
#[path = "../../../e2e/os/specs/desktop.rs"]
mod desktop;
#[path = "../../../e2e/os/specs/display.rs"]
mod display;
#[path = "../../../e2e/os/specs/files.rs"]
mod files;
#[path = "../../../e2e/os/specs/kernel.rs"]
mod kernel;
#[path = "../../../e2e/os/specs/monitor.rs"]
mod monitor;
#[path = "../../../e2e/os/specs/saver.rs"]
mod saver;
#[path = "../../../e2e/os/specs/services.rs"]
mod services;
#[path = "../../../e2e/os/specs/session.rs"]
mod session;
#[path = "../../../e2e/os/specs/settings.rs"]
mod settings;
#[path = "../../../e2e/site/specs/site.rs"]
mod site_specs;
#[path = "../../../e2e/os/specs/stage.rs"]
mod stage;
#[path = "../../../e2e/os/specs/storage.rs"]
mod storage;
#[path = "../../../e2e/os/specs/terminal.rs"]
mod terminal;
#[path = "../../../e2e/os/specs/theme.rs"]
mod theme;
#[path = "../../../e2e/os/specs/update.rs"]
mod update;

use cdp_proto::Cdp;
use fjson::Value;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A spec: drives the VM through `Ctx`, `Err` says what was wrong.
pub type Spec = fn(&mut Ctx) -> Result<(), String>;

/// Every spec, by its coverage id. `cargo xtask cdp` checks this list and
/// `e2e/os/coverage.json` name the same features.
pub const SPECS: &[(&str, Spec)] = &[
    ("boot.title", boot::title),
    ("boot.integrity", boot::integrity),
    ("boot.release", boot::release),
    ("boot.seal", boot::seal),
    ("boot.handoff", boot::handoff),
    ("kernel.panic_screen", kernel::panic_screen),
    ("kernel.last_panic", kernel::last_panic),
    ("boot.conductor", boot::conductor),
    ("services.restart", services::restart),
    ("log.read", services::read),
    ("devices.pci", services::pci),
    ("display.fill", services::fill),
    ("input.keys", services::keys),
    ("input.mouse", services::mouse),
    ("input.absent", services::absent),
    ("serial.echo", services::echo),
    ("serial.early", services::early),
    ("boot.timeline", boot::timeline),
    ("boot.fallback", boot::fallback),
    ("boot.trial", boot::trial),
    ("boot.gpt", boot::gpt),
    ("boot.gpt_fallback", boot::gpt_fallback),
    ("nvme.interlock", storage::interlock),
    ("fs.files", terminal::fs_files),
    ("fs.permissions", terminal::fs_permissions),
    ("fs.persists", terminal::fs_persists),
    ("memory.steady", terminal::memory_steady),
    ("accounts.lifecycle", accounts::lifecycle),
    ("accounts.throttle", accounts::throttle),
    ("accounts.password", accounts::password),
    ("accounts.key_gone", accounts::key_gone),
    ("accounts.export", accounts::export),
    ("accounts.import", accounts::import),
    ("welcome.form", session::welcome_form),
    ("login.form", session::login_form),
    ("accounts.login", session::login_time),
    ("login.restart", session::login_restart),
    ("login.shutdown", session::login_shutdown),
    ("settings.look", settings::look_spec),
    ("settings.hardware", settings::hardware),
    ("settings.about.licences", settings::about_licences),
    ("update.bad-signature", update::bad_signature),
    ("update.install", update::install),
    ("update.switch-back", update::switch_back),
    ("update.fallback", update::fallback),
    ("settings.keyboard", settings::keyboard),
    ("settings.accounts", settings::accounts),
    ("files.browse", files::browse),
    ("files.edit", files::edit),
    ("files.drag", files::drag_spec),
    ("files.copy.paste", files::copy_paste),
    ("files.copy.conflict", files::copy_conflict),
    ("files.copy.pause", files::copy_pause),
    ("files.copy.drag", files::copy_drag),
    ("bench.copy", files::bench),
    ("accounts.move_out", accounts::move_out),
    ("accounts.move", accounts::move_in),
    ("stage.windows", stage::windows),
    ("ui.cursor", stage::cursor),
    ("stage.doors", stage::doors),
    ("desktop.rack", desktop::rack),
    ("desktop.launcher", desktop::launcher),
    ("desktop.still", desktop::still),
    ("saver.starts_when_idle", saver::starts_when_idle),
    ("saver.ends_on_input", saver::ends_on_input),
    ("saver.reduce_motion", saver::reduce_motion),
    ("saver.bounce", saver::bounce),
    ("saver.block", saver::block),
    ("saver.swarm", saver::swarm),
    ("saver.warp", saver::warp),
    ("terminal.vt", terminal::vt),
    ("shell.builtins", terminal::builtins),
    ("monitor.tabs", monitor::tabs),
    ("cdp.freally", cdp_specs::freally),
    ("cdp.dom", cdp_specs::dom),
    ("desktop.sweep", desktop::sweep),
    ("apps.calculator", calculator::calculator),
    ("crisp.scales", crisp::scales),
    ("monitor.sweep", monitor::sweep),
    ("a11y.keyboard", a11y::keyboard),
    ("theme.both", theme::both),
    ("display.native", display::native),
    ("monitor.devices", monitor::devices),
    ("monitor.last_panic", monitor::last_panic),
    ("log.screen", services::log_screen),
    ("log.screen_hands_over", services::log_screen_hands_over),
];

/// The docs-site leg's specs (`cdp-runner --site DIR`).
/// A site spec: drives headless Chrome through `SiteCtx`.
pub type SiteSpec = fn(&mut site::SiteCtx) -> Result<(), String>;

pub const SITE_SPECS: &[(&str, SiteSpec)] = &[
    ("site.pages", site_specs::pages),
    ("site.theme", site_specs::theme),
    ("site.no_downloads", site_specs::no_downloads),
    ("site.no_private", site_specs::no_private),
    ("site.cursor", site_specs::cursor),
    ("site.toggles", site_specs::toggles),
    ("site.documents", site_specs::documents),
];

/// A screen a spec expects, drawn on the host by `fscreen` to compare.
pub enum Expect {
    Title {
        note: &'static str,
    },
    Integrity {
        reason: &'static str,
    },
    Panic {
        lines: [String; 3],
    },
    /// One colour everywhere, `0xRRGGBB`.
    Fill(u32),
    /// The desktop: the dark wallpaper everywhere but the fleet cursor's
    /// square at the screen's centre, where the ring must show (TASK-058),
    /// and the Rack's faceplate along the foot (TASK-059, TASK-052; its keys
    /// and clock are `@desktop.rack`'s).
    Desktop,
}

/// The Rack's height at 100 %.
const RACK_H: usize = 56;

/// The fleet cursor's square, centred on the screen: 64 × 64.
const CURSOR_BOX: usize = 32;

/// Clearly Freally green: green the strongest, well above blue, clear of red.
pub fn greenish(r: u8, g: u8, b: u8) -> bool {
    g as u32 >= r as u32 + 30 && g as u32 > b as u32 + 60
}

pub struct Ctx {
    pub cdp: Cdp,
    pub com1: PathBuf,
    pub failures: PathBuf,
    pub spec: String,
    allowed_errors: Vec<String>,
    /// The e2e test door (COM3's socket), when the setup has one.
    pub com3: Option<String>,
    /// What the guard lets this spec do: restart services, file faults; and
    /// whether it rebooted the VM (the guard's counts begin again).
    allow_restarts: bool,
    allow_faults: bool,
    rebooted: bool,
    /// Whether this setup's boot opened the test door (asked once).
    door_open: Option<bool>,
    /// The controls shown and worked, for the inventory; when keys last
    /// asked which control had the keyboard.
    pub inv: inventory::Inventory,
    key_probe: Option<Instant>,
    /// What the spec's line should also say (printed under it).
    notes: Vec<String>,
}

impl Ctx {
    /// A click is about to land: the control under the pointer is worked.
    pub fn note_press(&mut self) {
        if self.door_open != Some(true) {
            return;
        }
        if let Ok(r) = self.cdp.call("Freally.nodeAt", Value::Obj(Vec::new()))
            && let Some(n) = r.get("node").and_then(inventory::name)
        {
            self.inv.worked.insert(n);
        }
    }

    /// A key is about to go down: the control with the keyboard is worked
    /// (asked at most every 400 ms: a word typed is one question).
    pub fn note_key(&mut self) {
        if self.door_open != Some(true) || self.key_probe.is_some_and(|t| t.elapsed() < Duration::from_millis(400)) {
            return;
        }
        self.key_probe = Some(Instant::now());
        if let Ok(r) = self.cdp.call("Freally.focusedNode", Value::Obj(Vec::new()))
            && let Some(n) = r.get("node").and_then(inventory::name)
        {
            self.inv.worked.insert(n);
        }
    }

    /// Every request ordinal the services have been sent so far.
    fn note_served(&mut self) {
        if let Ok(r) = self.cdp.call("Freally.ordinals", Value::Obj(Vec::new())) {
            for s in r.get("services").and_then(Value::as_array).unwrap_or_default() {
                let name = s.get("service").and_then(Value::as_str).unwrap_or("");
                for o in s.get("ordinals").and_then(Value::as_array).unwrap_or_default() {
                    let pair = o.as_array().unwrap_or_default();
                    if let (Some(ord), Some(n)) =
                        (pair.first().and_then(Value::as_i64), pair.get(1).and_then(Value::as_i64))
                        && n > 0
                    {
                        self.inv.served.insert(format!("{name} {ord}"));
                    }
                }
            }
        }
    }

    /// Every control cdpd has seen so far, for the inventory.
    fn note_shown(&mut self) {
        if let Ok(r) = self.cdp.call("Freally.inventory", Value::Obj(Vec::new())) {
            for v in r.get("seen").and_then(Value::as_array).unwrap_or_default() {
                if let Some(n) = inventory::name(v) {
                    self.inv.shown.insert(n);
                }
            }
        }
    }

    /// This spec restarts services on purpose: the guard lets it.
    pub fn allow_restarts(&mut self) {
        self.allow_restarts = true;
    }

    /// This spec makes something crash on purpose: the guard lets it.
    pub fn allow_faults(&mut self) {
        self.allow_faults = true;
    }

    /// What the spec begins with, for the guard: nothing without an open
    /// test door (a setup without one, or a boot not that far yet).
    fn watch(&mut self) -> Option<guard::Watch> {
        self.com3.as_ref()?;
        const OPEN: &str = "conductor: test door open on COM3";
        if self.door_open.is_none() {
            // The first spec: give the boot up to 10 s to open the door (one
            // that panics never will).
            let start = Instant::now();
            while !self.log().contains(OPEN) && start.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(100));
            }
            self.door_open = Some(self.log().contains(OPEN));
        }
        if !self.log().contains(OPEN) {
            return None;
        }
        let f = self.cdp.call("Freally.getFaults", Value::obj([("sinceSeq", 0u32.into())])).ok()?;
        let p = self.cdp.call("Freally.getProcesses", Value::Obj(Vec::new())).ok()?;
        let l = self.cdp.call("Log.read", Value::obj([("cursor", Value::Num(9e15)), ("limit", 1u32.into())])).ok()?;
        Some(guard::Watch {
            log: l.get("newest").and_then(Value::as_i64)? as u64 + 1,
            fault: f.get("lastSeq").and_then(Value::as_i64)? as u64,
            restarts: guard::restarts(&p),
        })
    }

    /// The guard's half through cdpd: errors logged, kernel warnings, new
    /// faults and restarts since `w`.
    fn guard(&mut self, w: &guard::Watch) -> Result<(), String> {
        let mut bad = Vec::new();
        let mut cursor = w.log;
        for _ in 0..20 {
            let params =
                Value::obj([("cursor", cursor.into()), ("limit", 500u32.into()), ("minLevel", "warning".into())]);
            let r = self.guard_call("Log.read", params).map_err(|e| format!("the guard: Log.read: {e}"))?;
            let entries = r.get("entries").and_then(Value::as_array).unwrap_or_default().to_vec();
            bad.extend(guard::bad_records(&entries, &self.allowed_errors));
            let next = r.get("next").and_then(Value::as_i64).unwrap_or(0) as u64;
            if entries.is_empty() || next <= cursor {
                break;
            }
            cursor = next;
        }
        if !bad.is_empty() {
            return Err(format!("the guard: logged {}", bad.join(" | ")));
        }
        if !self.allow_faults {
            let f = self
                .guard_call("Freally.getFaults", Value::obj([("sinceSeq", w.fault.into())]))
                .map_err(|e| format!("the guard: getFaults: {e}"))?;
            let new = f.get("faults").and_then(Value::as_array).unwrap_or_default();
            if !new.is_empty() {
                return Err(format!("the guard: new fault reports {}", Value::Arr(new.to_vec()).to_json()));
            }
        }
        if !self.allow_restarts {
            let p = self
                .guard_call("Freally.getProcesses", Value::Obj(Vec::new()))
                .map_err(|e| format!("the guard: {e}"))?;
            let again = guard::restarted(&w.restarts, &guard::restarts(&p));
            if !again.is_empty() {
                return Err(format!("the guard: restarted {}", again.join(", ")));
            }
        }
        Ok(())
    }

    /// One of the guard's calls through the test door. A door still busy from
    /// a heavy spec (`@serial.echo`'s 256 KB on a loaded host) can sit out
    /// cdp-host's 15 s; it's asked once more, and the spec's line says so.
    fn guard_call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        match self.cdp.call(method, params.clone()) {
            Err(e) if e.contains("went quiet") => {
                self.notes.push(format!("the test door was quiet for the guard's {method}; asked again"));
                self.cdp.call(method, params)
            }
            r => r,
        }
    }

    /// `ERROR` lines containing any of `patterns` don't fail this spec.
    pub fn allow_errors(&mut self, patterns: &[&str]) {
        self.allowed_errors.extend(patterns.iter().map(|p| p.to_string()));
    }

    pub fn screenshot(&mut self) -> Result<fimage::png::Decoded, String> {
        let r = self.cdp.call("Page.captureScreenshot", Value::obj([("format", "png".into())]))?;
        let b64 = r.get("data").and_then(Value::as_str).ok_or("no screenshot data")?;
        let png = fjson::base64::decode(b64).ok_or("screenshot isn't base64")?;
        fimage::png::decode_stored(&png).map_err(|e| format!("screenshot PNG: {e}"))
    }

    /// Waits (up to 20 s) for the screen to equal `e`, pixel for pixel.
    pub fn expect_screen(&mut self, e: Expect) -> Result<(), String> {
        let e = &e;
        let start = Instant::now();
        loop {
            let shot = self.screenshot()?;
            let (w, h) = (shot.w as usize, shot.h as usize);
            let mut px = vec![0u32; w * h];
            let mut c = fscreen::Canvas { px: &mut px, w, h };
            let version = env!("CARGO_PKG_VERSION");
            match *e {
                Expect::Title { note } => fscreen::title(&mut c, version, note),
                Expect::Integrity { reason } => fscreen::integrity(&mut c, reason),
                Expect::Panic { ref lines } => {
                    fscreen::panic(&mut c, &[lines[0].as_str(), lines[1].as_str(), lines[2].as_str()])
                }
                Expect::Fill(rgb) => {
                    c.px.fill(rgb);
                    fscreen::Rect { x: 0, y: 0, w: w as i32, h: h as i32 }
                }
                Expect::Desktop => {
                    for (i, p) in c.px.iter_mut().enumerate() {
                        *p = art::ground("dark", (i % w) as i32, (i / w) as i32);
                    }
                    fscreen::Rect { x: 0, y: 0, w: w as i32, h: h as i32 }
                }
            };
            let (cx, cy) = (w / 2, h / 2);
            let cursor = matches!(e, Expect::Desktop);
            let mut diff = 0usize;
            for (i, p) in px.iter().enumerate() {
                if cursor && (i % w).abs_diff(cx) < CURSOR_BOX && (i / w).abs_diff(cy) < CURSOR_BOX {
                    continue;
                }
                if cursor && i / w >= h - RACK_H {
                    continue;
                }
                let a = &shot.pixels[i * 3..i * 3 + 3];
                if [(p >> 16) as u8, (p >> 8) as u8, *p as u8] != [a[0], a[1], a[2]] {
                    diff += 1;
                }
            }
            // The ring, 16 px right of the centre (its line at radius 15.5 to 17).
            let ring = |x: usize, y: usize| {
                let i = (y * w + x) * 3;
                greenish(shot.pixels[i], shot.pixels[i + 1], shot.pixels[i + 2])
            };
            if cursor && !(ring(cx + 16, cy) && ring(cx - 16, cy) && ring(cx, cy + 16) && ring(cx, cy - 16)) {
                diff += 1;
            }
            // The Rack: its faceplate's top edge, and 6 px below it (above the
            // line the keys, the clock and a guest's notice sit on).
            let at = |x: usize, y: usize| {
                let i = (y * w + x) * 3;
                (shot.pixels[i] as u32) << 16 | (shot.pixels[i + 1] as u32) << 8 | shot.pixels[i + 2] as u32
            };
            let plate = |y: usize| art::rack("dark", (w as u32, RACK_H as u32), cx as i32, y as i32);
            if cursor && (at(cx, h - RACK_H) != plate(0) || at(cx, h - RACK_H + 6) != plate(6)) {
                diff += 1;
            }
            if diff == 0 {
                return Ok(());
            }
            if start.elapsed() > Duration::from_secs(20) {
                self.save_failure(&shot, &px)?;
                return Err(format!(
                    "{diff} of {} pixels differ from the expected screen (images in {})",
                    w * h,
                    self.failures.display()
                ));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// Keeps a screenshot as `<failures>/../shots/<name>.png`: a picture of a
    /// surface for the owner, from the real screen.
    pub fn keep_shot(&mut self, name: &str) -> Result<(), String> {
        let shot = self.screenshot()?;
        let dir = self.failures.parent().map(|p| p.join("shots")).unwrap_or_else(|| self.failures.join("shots"));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(format!("{name}.png")), fimage::png::encode(shot.w, shot.h, 3, &shot.pixels))
            .map_err(|e| e.to_string())
    }

    /// Writes actual, expected and diff PNGs for a failed comparison.
    fn save_failure(&self, shot: &fimage::png::Decoded, expected: &[u32]) -> Result<(), String> {
        std::fs::create_dir_all(&self.failures).map_err(|e| e.to_string())?;
        let (w, h) = (shot.w, shot.h);
        let mut exp = Vec::with_capacity(expected.len() * 3);
        let mut diff = Vec::with_capacity(expected.len() * 3);
        for (i, p) in expected.iter().enumerate() {
            let e = [(p >> 16) as u8, (p >> 8) as u8, *p as u8];
            exp.extend_from_slice(&e);
            let a = &shot.pixels[i * 3..i * 3 + 3];
            diff.extend_from_slice(if a == e { &[0, 0, 0] } else { &[255, 0, 255] });
        }
        let save = |name: &str, px: &[u8]| {
            std::fs::write(self.failures.join(format!("{}-{name}.png", self.spec)), fimage::png::encode(w, h, 3, px))
                .map_err(|e| e.to_string())
        };
        save("actual", &shot.pixels)?;
        save("expected", &exp)?;
        save("diff", &diff)
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(&self.com1).unwrap_or_default().replace('\r', "")
    }

    /// COM1 has a line containing `text` (waiting up to 10 s for it).
    pub fn expect_log(&self, text: &str) -> Result<(), String> {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            if self.log().contains(text) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(format!("COM1 never said {text:?}"))
    }

    /// The first COM1 line containing `text`.
    pub fn log_line_with(&self, text: &str) -> Option<String> {
        self.log().lines().find(|l| l.contains(text)).map(String::from)
    }

    /// Sends `cmd` to the e2e test door (conductor, on COM3) and returns its
    /// answer's lines, up to `end` (waiting up to 10 s).
    /// A test door command. Stage registers only once its first frame is
    /// up, so a stage command asked just after boot is asked again until it
    /// is (at most 10 s): "stage isn't up" is never a spec's answer.
    pub fn door(&self, cmd: &str) -> Result<Vec<String>, String> {
        let start = Instant::now();
        loop {
            let lines = self.door_once(cmd)?;
            let early = matches!(lines.as_slice(), [only] if only == "error: stage isn't up");
            if !early || start.elapsed() > Duration::from_secs(10) {
                return Ok(lines);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn door_once(&self, cmd: &str) -> Result<Vec<String>, String> {
        use std::io::{BufRead, Write};
        let stream = self.door_stream()?;
        let mut w = stream.try_clone().map_err(|e| e.to_string())?;
        w.write_all(format!("{cmd}\n").as_bytes()).map_err(|e| e.to_string())?;
        let mut lines = Vec::new();
        for line in std::io::BufReader::new(stream).lines() {
            let line = line.map_err(|e| format!("the test door went quiet after {lines:?}: {e}"))?;
            let line = line.trim().to_string();
            if line == "end" {
                return Ok(lines);
            }
            if !line.is_empty() {
                lines.push(line);
            }
        }
        Err(format!("the test door closed after {lines:?}"))
    }

    /// COM3's socket (waiting up to 10 s for it), reads timing out after 10 s.
    fn door_stream(&self) -> Result<std::net::TcpStream, String> {
        let addr = self.com3.as_deref().ok_or("this setup has no test door (e2e only)")?;
        let start = Instant::now();
        let stream = loop {
            match std::net::TcpStream::connect(addr) {
                Ok(s) => break s,
                Err(e) if start.elapsed() > Duration::from_secs(10) => return Err(format!("the test door: {e}")),
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        };
        stream.set_read_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
        Ok(stream)
    }

    /// The door's `echo`: sends `data` and returns what came back, writing
    /// and reading at once (a serial line runs both ways). The door first
    /// stops reading for `stall_ms`, so the driver must hold the line.
    pub fn door_echo(&self, data: &[u8], stall_ms: u64) -> Result<Vec<u8>, String> {
        use std::io::{BufRead, Read, Write};
        let stream = self.door_stream()?;
        let mut w = stream.try_clone().map_err(|e| e.to_string())?;
        w.write_all(format!("echo {} {stall_ms}\n", data.len()).as_bytes()).map_err(|e| e.to_string())?;
        let mut r = std::io::BufReader::new(stream);
        let mut line = String::new();
        r.read_line(&mut line).map_err(|e| format!("no answer to echo: {e}"))?;
        if line.trim() != "ok" {
            return Err(format!("echo was answered {:?}", line.trim()));
        }
        let payload = data.to_vec();
        let writer = std::thread::spawn(move || w.write_all(&payload).map_err(|e| e.to_string()));
        let mut back = vec![0u8; data.len()];
        let mut got = 0;
        while got < back.len() {
            match r.read(&mut back[got..]) {
                Ok(0) => return Err(format!("the test door closed after {got} of {} bytes", data.len())),
                Ok(n) => got += n,
                Err(e) => return Err(format!("the echo stopped after {got} of {} bytes: {e}", data.len())),
            }
        }
        writer.join().map_err(|_| "the writer panicked".to_string())??;
        line.clear();
        r.read_line(&mut line).map_err(|e| format!("no end after the echo: {e}"))?;
        if line.trim() != "end" {
            return Err(format!("the echo ended with {:?}, not end", line.trim()));
        }
        Ok(back)
    }

    /// A line printed under the spec's result (a measurement, say).
    pub fn note(&mut self, line: &str) {
        self.notes.push(String::from(line));
    }

    /// The VM restarted or powered off by itself (Restart, Shut down;
    /// TASK-102): the guard's counts begin again, as after `reset`.
    pub fn rebooted_by_guest(&mut self) {
        self.rebooted = true;
    }

    /// A warm reboot (RAM kept), as a restart button does.
    /// Resets the VM and waits for the next boot's first line: up to 60 s,
    /// as for the first boot (a loaded host can take far longer than a spec's
    /// 10 s to bring the firmware back up).
    pub fn reset(&mut self) -> Result<(), String> {
        self.rebooted = true;
        let boots = self.log_count("freally-boot ");
        self.cdp.call("Page.reload", Value::Obj(Vec::new()))?;
        let start = Instant::now();
        while self.log_count("freally-boot ") <= boots {
            if start.elapsed() > Duration::from_secs(60) {
                return Err(String::from("the VM didn't boot again within 60 s of its reset"));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }

    /// Every COM1 line containing `text`.
    pub fn log_lines_with(&self, text: &str) -> Vec<String> {
        self.log().lines().filter(|l| l.contains(text)).map(String::from).collect()
    }

    /// How many COM1 lines contain `text`.
    pub fn log_count(&self, text: &str) -> usize {
        self.log().lines().filter(|l| l.contains(text)).count()
    }

    /// Waits up to 10 s for at least `n` COM1 lines containing `text`.
    pub fn expect_log_count(&self, text: &str, n: usize) -> Result<(), String> {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            if self.log_count(text) >= n {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(format!("COM1 said {text:?} {} times, not {n}", self.log_count(text)))
    }

    pub fn expect_no_log(&self, text: &str) -> Result<(), String> {
        if self.log().contains(text) { Err(format!("COM1 said {text:?}")) } else { Ok(()) }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    if args.iter().any(|a| a == "--list") {
        for (id, _) in SPECS {
            println!("{id}");
        }
        return;
    }
    if args.iter().any(|a| a == "--list-site") {
        for (id, _) in SITE_SPECS {
            println!("{id}");
        }
        return;
    }
    if let Some(dir) = arg("--site") {
        std::process::exit(site_leg(PathBuf::from(dir), &arg("--specs").unwrap_or_default()));
    }
    if args.iter().any(|a| a == "--boot-check") {
        let com1 = PathBuf::from(arg("--com1-log").unwrap_or_default());
        let r = connect(&arg("--endpoint").unwrap_or_default()).and_then(|mut c| bootcheck::run(&mut c, &com1));
        match r {
            Ok(v) => println!("{}", v.to_json()),
            Err(e) => {
                eprintln!("cdp-runner: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    let (Some(endpoint), Some(com1), Some(specs)) = (arg("--endpoint"), arg("--com1-log"), arg("--specs")) else {
        eprintln!("cdp-runner --endpoint HOST:PORT --com1-log PATH --specs id,id [--failures DIR] | --list");
        std::process::exit(2);
    };
    let failures = PathBuf::from(arg("--failures").unwrap_or_else(|| "cdp-failures".into()));
    let cdp = match connect(&endpoint) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cdp-runner: can't connect to {endpoint}: {e}");
            std::process::exit(1);
        }
    };
    let mut ctx = Ctx {
        cdp,
        com1: PathBuf::from(com1),
        failures,
        spec: String::new(),
        allowed_errors: Vec::new(),
        com3: arg("--com3"),
        allow_restarts: false,
        allow_faults: false,
        rebooted: false,
        door_open: None,
        inv: inventory::Inventory::default(),
        key_probe: None,
        notes: Vec::new(),
    };
    // A cold host (WSL just restarted, its caches empty) can take far longer
    // than a spec's 10 s to start the VM: wait up to 60 s for the
    // bootloader's first line (every image prints it, refused or not).
    let boot = Instant::now();
    while !ctx.log().contains("freally-boot ") && boot.elapsed() < Duration::from_secs(60) {
        std::thread::sleep(Duration::from_millis(200));
    }
    let mut red = 0;
    let wanted: Vec<&str> = specs.split(',').filter(|s| !s.is_empty()).collect();
    for id in &wanted {
        let Some((_, f)) = SPECS.iter().find(|(s, _)| s == id) else {
            println!("▶ @{id} … fail (0.0s)\n    no such spec");
            red += 1;
            continue;
        };
        ctx.spec = id.to_string();
        ctx.allowed_errors.clear();
        (ctx.allow_restarts, ctx.allow_faults, ctx.rebooted) = (false, false, false);
        let printed = ctx.log().len();
        let watch = ctx.watch();
        let start = Instant::now();
        let mut result = f(&mut ctx);
        if result.is_ok() {
            let log = ctx.log();
            result = guard::check(log.get(printed..).unwrap_or(&log), &ctx.allowed_errors);
        }
        if result.is_ok()
            && !ctx.rebooted
            && let Some(w) = &watch
        {
            result = ctx.guard(w);
        }
        if watch.is_some() && !ctx.rebooted {
            ctx.note_shown();
            ctx.note_served();
        }
        let secs = start.elapsed().as_secs_f64();
        match result {
            Ok(()) => println!("▶ @{id} … pass ({secs:.1}s)"),
            Err(e) => {
                red += 1;
                println!("▶ @{id} … fail ({secs:.1}s)\n    {e}");
            }
        }
        for n in ctx.notes.drain(..) {
            println!("    note: {n}");
        }
    }
    if let Some(out) = arg("--inventory-out")
        && let Err(e) = std::fs::write(&out, ctx.inv.to_json().to_json())
    {
        eprintln!("cdp-runner: can't write {out}: {e}");
    }
    std::process::exit(if red == 0 { 0 } else { 1 });
}

/// The site leg: serve the built site, start headless Chrome, run the specs.
fn site_leg(root: PathBuf, specs: &str) -> i32 {
    let base = match site::serve(root.clone()) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cdp-runner: can't serve {}: {e}", root.display());
            return 1;
        }
    };
    let chrome = match site::launch() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cdp-runner: {e}");
            return 1;
        }
    };
    let mut ctx = match connect(&chrome.endpoint).and_then(|cdp| site::SiteCtx::new(cdp, base, root)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cdp-runner: can't drive Chrome: {e}");
            return 1;
        }
    };
    let mut red = 0;
    for id in specs.split(',').filter(|s| !s.is_empty()) {
        let start = Instant::now();
        let result = match SITE_SPECS.iter().find(|(s, _)| *s == id) {
            Some((_, f)) => f(&mut ctx),
            None => Err("no such spec".into()),
        };
        let secs = start.elapsed().as_secs_f64();
        match result {
            Ok(()) => println!("▶ @{id} … pass ({secs:.1}s)"),
            Err(e) => {
                red += 1;
                println!("▶ @{id} … fail ({secs:.1}s)\n    {e}");
            }
        }
    }
    drop(chrome);
    if red == 0 { 0 } else { 1 }
}

/// Connects, retrying while cdp-host starts.
fn connect(endpoint: &str) -> Result<Cdp, String> {
    let start = Instant::now();
    loop {
        match Cdp::connect(endpoint) {
            Ok(c) => return Ok(c),
            Err(e) if start.elapsed() > Duration::from_secs(30) => return Err(e.to_string()),
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}
