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

mod guard;
mod site;

#[path = "../../../e2e/os/specs/boot.rs"]
mod boot;
#[path = "../../../e2e/os/specs/kernel.rs"]
mod kernel;
#[path = "../../../e2e/os/specs/services.rs"]
mod services;
#[path = "../../../e2e/site/specs/site.rs"]
mod site_specs;

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
}

pub struct Ctx {
    pub cdp: Cdp,
    pub com1: PathBuf,
    pub failures: PathBuf,
    pub spec: String,
    allowed_errors: Vec<String>,
    /// The e2e test door (COM3's socket), when the setup has one.
    pub com3: Option<String>,
}

impl Ctx {
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
            };
            let mut diff = 0usize;
            for (i, p) in px.iter().enumerate() {
                let a = &shot.pixels[i * 3..i * 3 + 3];
                if [(p >> 16) as u8, (p >> 8) as u8, *p as u8] != [a[0], a[1], a[2]] {
                    diff += 1;
                }
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

    fn log(&self) -> String {
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
    pub fn door(&self, cmd: &str) -> Result<Vec<String>, String> {
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

    /// A warm reboot (RAM kept), as a restart button does.
    pub fn reset(&mut self) -> Result<(), String> {
        self.cdp.call("Page.reload", Value::Obj(Vec::new())).map(|_| ())
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
    };
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
        let start = Instant::now();
        let mut result = f(&mut ctx);
        if result.is_ok() {
            result = guard::check(&ctx.log(), &ctx.allowed_errors);
        }
        let secs = start.elapsed().as_secs_f64();
        match result {
            Ok(()) => println!("▶ @{id} … pass ({secs:.1}s)"),
            Err(e) => {
                red += 1;
                println!("▶ @{id} … fail ({secs:.1}s)\n    {e}");
            }
        }
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
