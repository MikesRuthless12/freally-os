// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask showcase [tour]`: films the real OS. Every feature is shown in
//! Freally OS itself, never a mock-up (the fleet's rule).
//!
//! A tour (`showcase/tours/*.ron`) boots its flavour headless and runs steps:
//! `Record(ms: N)` films N ms, `Key(code: "KeyA")` presses a key, `Wait(ms: N)`
//! waits without filming. Frames are QMP screendumps on a 30 fps clock (a slow
//! dump repeats the last frame, so the clock stays true); `ffmpeg` (a host tool)
//! makes the MP4 in `target/showcase/`. From Phase 3 the fleet cursor is drawn
//! by the OS, so it's in every frame.

use crate::run::VmOptions;
use crate::{build, pack, util};
use cdp_proto::qmp::{self, Qmp};
use fjson::Value;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const FPS: u32 = 30;

#[derive(Debug, PartialEq)]
pub enum Step {
    Record(u64),
    Wait(u64),
    Key(String),
}

#[derive(Debug, PartialEq)]
pub struct Tour {
    pub name: String,
    pub flavour: String,
    pub steps: Vec<Step>,
}

/// Parses the small RON subset tours use: `Tour(name: "…", flavour: "…", steps: [Step(key: value), …])`.
pub fn parse(text: &str) -> Result<Tour, String> {
    let src: String = text.lines().map(|l| l.split("//").next().unwrap_or("")).collect::<Vec<_>>().join("\n");
    let mut t = Tokens { s: src.as_bytes(), i: 0 };
    t.word("Tour")?;
    t.punct(b'(')?;
    let (mut name, mut flavour, mut steps) = (None, None, Vec::new());
    while !t.peek(b')') {
        let key = t.ident()?;
        t.punct(b':')?;
        match key.as_str() {
            "name" => name = Some(t.string()?),
            "flavour" => flavour = Some(t.string()?),
            "steps" => {
                t.punct(b'[')?;
                while !t.peek(b']') {
                    let kind = t.ident()?;
                    t.punct(b'(')?;
                    let field = t.ident()?;
                    t.punct(b':')?;
                    let step = match (kind.as_str(), field.as_str()) {
                        ("Record", "ms") => Step::Record(t.number()?),
                        ("Wait", "ms") => Step::Wait(t.number()?),
                        ("Key", "code") => Step::Key(t.string()?),
                        _ => return Err(format!("unknown step {kind}({field}: …)")),
                    };
                    t.punct(b')')?;
                    steps.push(step);
                    t.comma();
                }
                t.punct(b']')?;
            }
            other => return Err(format!("unknown tour field {other:?}")),
        }
        t.comma();
    }
    t.punct(b')')?;
    Ok(Tour { name: name.ok_or("a tour needs a name")?, flavour: flavour.unwrap_or_else(|| "dev".into()), steps })
}

struct Tokens<'a> {
    s: &'a [u8],
    i: usize,
}

impl Tokens<'_> {
    fn ws(&mut self) {
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }
    fn peek(&mut self, c: u8) -> bool {
        self.ws();
        self.s.get(self.i) == Some(&c)
    }
    fn punct(&mut self, c: u8) -> Result<(), String> {
        if self.peek(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected {:?} at byte {}", c as char, self.i))
        }
    }
    fn comma(&mut self) {
        if self.peek(b',') {
            self.i += 1;
        }
    }
    fn ident(&mut self) -> Result<String, String> {
        self.ws();
        let start = self.i;
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') {
            self.i += 1;
        }
        if start == self.i {
            return Err(format!("expected a name at byte {start}"));
        }
        Ok(String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }
    fn word(&mut self, w: &str) -> Result<(), String> {
        let got = self.ident()?;
        if got == w { Ok(()) } else { Err(format!("expected {w}, found {got}")) }
    }
    fn string(&mut self) -> Result<String, String> {
        self.punct(b'"')?;
        let start = self.i;
        while self.s.get(self.i).is_some_and(|&c| c != b'"') {
            self.i += 1;
        }
        let v = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
        self.punct(b'"')?;
        Ok(v)
    }
    fn number(&mut self) -> Result<u64, String> {
        self.ws();
        let start = self.i;
        while self.s.get(self.i).is_some_and(u8::is_ascii_digit) {
            self.i += 1;
        }
        std::str::from_utf8(&self.s[start..self.i])
            .ok()
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| format!("expected a number at byte {start}"))
    }
}

/// Films `ms` milliseconds into `frames/`, numbering from `next`.
fn record(q: &mut Qmp, frames: &Path, next: &mut u32, ms: u64) -> Result<(), String> {
    let start = Instant::now();
    let tick = Duration::from_secs_f64(1.0 / FPS as f64);
    let ticks = (ms * FPS as u64 / 1000) as u32;
    let first = *next;
    while *next < first + ticks {
        let path = frames.join(format!("{:05}.ppm", *next));
        q.execute("screendump", Some(Value::obj([("filename", path.display().to_string().into())])))?;
        *next += 1;
        // A dump slower than a tick fills the missed ticks with this frame.
        let due = ((start.elapsed().as_secs_f64() / tick.as_secs_f64()) as u32).min(ticks);
        while *next < first + due {
            std::fs::hard_link(&path, frames.join(format!("{:05}.ppm", *next))).map_err(|e| e.to_string())?;
            *next += 1;
        }
        let target = tick * (*next - first);
        if let Some(wait) = target.checked_sub(start.elapsed()) {
            std::thread::sleep(wait);
        }
    }
    Ok(())
}

pub fn film(tour: &Tour) -> Result<std::path::PathBuf, String> {
    let flavour = pack::parse_flavour(&tour.flavour)?;
    let out = util::target_dir().join("showcase");
    let frames = out.join(format!("{}-frames", tour.name));
    let _ = std::fs::remove_dir_all(&frames);
    std::fs::create_dir_all(&frames).map_err(|e| e.to_string())?;
    let built = build::build(flavour)?;
    let mut o = VmOptions::new(flavour, &format!("showcase-{}", tour.name));
    o.headless = true;
    o.port_offset = 400;
    let mut vm = crate::run::start_built(&o, &built)?;
    let result = (|| {
        let mut q = Qmp::connect(&o.qmp_socket(), Duration::from_secs(20))?;
        let mut next = 0u32;
        for step in &tour.steps {
            match step {
                Step::Record(ms) => record(&mut q, &frames, &mut next, *ms)?,
                Step::Wait(ms) => std::thread::sleep(Duration::from_millis(*ms)),
                Step::Key(code) => qmp::tap(&mut q, &qmp::qcode(code).ok_or_else(|| format!("no key {code}"))?)?,
            }
        }
        Ok::<u32, String>(next)
    })();
    let _ = vm.kill();
    let _ = vm.wait();
    let n = result?;
    let mp4 = out.join(format!("{}.mp4", tour.name));
    util::run_ok(
        Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-framerate", &FPS.to_string(), "-i"])
            .arg(frames.join("%05d.ppm"))
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "18"])
            .arg(&mp4),
    )?;
    let _ = std::fs::remove_dir_all(&frames);
    println!("showcase: {} → {} ({n} frames, {:.1} s at {FPS} fps)", tour.name, mp4.display(), n as f64 / FPS as f64);
    Ok(mp4)
}

pub fn showcase_cmd(args: &[String]) -> Result<(), String> {
    let dir = util::repo_root().join("showcase/tours");
    let mut tours = Vec::new();
    for e in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten() {
        if e.path().extension().is_some_and(|x| x == "ron") {
            let text = std::fs::read_to_string(e.path()).map_err(|e| e.to_string())?;
            tours.push(parse(&text).map_err(|err| format!("{}: {err}", e.path().display()))?);
        }
    }
    tours.sort_by(|a, b| a.name.cmp(&b.name));
    let wanted: Vec<&Tour> = tours.iter().filter(|t| args.first().is_none_or(|w| *w == t.name)).collect();
    if wanted.is_empty() {
        return Err(format!("no tour named {:?}", args.first()));
    }
    for t in wanted {
        film(t)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_tour() {
        let t = parse(
            "// a comment\nTour(\n  name: \"demo\",\n  flavour: \"e2e\",\n  steps: [Record(ms: 500), Key(code: \"KeyM\"), Wait(ms: 10),],\n)\n",
        )
        .unwrap();
        assert_eq!(t.name, "demo");
        assert_eq!(t.flavour, "e2e");
        assert_eq!(t.steps, vec![Step::Record(500), Step::Key("KeyM".into()), Step::Wait(10)]);
    }

    #[test]
    fn refuses_unknown_steps() {
        assert!(parse("Tour(name: \"x\", steps: [Dance(ms: 1)])").is_err());
    }
}
