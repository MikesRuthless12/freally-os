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

#[path = "../../../e2e/os/specs/boot.rs"]
mod boot;
#[path = "../../../e2e/os/specs/kernel.rs"]
mod kernel;

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
];

/// A screen a spec expects, drawn on the host by `fscreen` to compare.
pub enum Expect {
    Title { note: &'static str },
    Integrity { reason: &'static str },
    Panic { lines: [String; 3] },
}

pub struct Ctx {
    pub cdp: Cdp,
    pub com1: PathBuf,
    pub failures: PathBuf,
    pub spec: String,
    allowed_errors: Vec<String>,
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
    let mut ctx = Ctx { cdp, com1: PathBuf::from(com1), failures, spec: String::new(), allowed_errors: Vec::new() };
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
