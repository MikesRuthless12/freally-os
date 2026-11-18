// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask showcase [tour]`: films the real OS. Every feature is shown in
//! Freally OS itself, never a mock-up (the fleet's rule).
//!
//! A tour (`showcase/tours/*.ron`) boots its flavour headless and runs steps:
//! `Record(ms: N)` films N ms, `Key(code: "KeyA")` presses a key (or a chord,
//! `"MetaLeft+KeyM"`, pressed in order and let go in reverse), `Type(text:
//! "ps\n")` types, filming each key as it lands, and `Wait(ms: N)` waits
//! without filming. `stopwatch: true` burns a stopwatch from power on into the
//! film (a measurement on the real footage, in Noto Sans Mono). Frames are QMP screendumps on a 30 fps clock (a slow
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
    Type(String),
}

#[derive(Debug, PartialEq)]
pub struct Tour {
    pub name: String,
    pub flavour: String,
    pub steps: Vec<Step>,
    pub stopwatch: bool,
}

/// Parses the small RON subset tours use: `Tour(name: "…", flavour: "…", steps: [Step(key: value), …])`.
pub fn parse(text: &str) -> Result<Tour, String> {
    let src: String = text.lines().map(|l| l.split("//").next().unwrap_or("")).collect::<Vec<_>>().join("\n");
    let mut t = Tokens { s: src.as_bytes(), i: 0 };
    t.word("Tour")?;
    t.punct(b'(')?;
    let (mut name, mut flavour, mut steps, mut stopwatch) = (None, None, Vec::new(), false);
    while !t.peek(b')') {
        let key = t.ident()?;
        t.punct(b':')?;
        match key.as_str() {
            "name" => name = Some(t.string()?),
            "flavour" => flavour = Some(t.string()?),
            "stopwatch" => stopwatch = t.ident()? == "true",
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
                        ("Type", "text") => Step::Type(t.string()?),
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
    Ok(Tour {
        name: name.ok_or("a tour needs a name")?,
        flavour: flavour.unwrap_or_else(|| "dev".into()),
        steps,
        stopwatch,
    })
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
        // `\n` is Enter (a `Type` step's).
        let v = String::from_utf8_lossy(&self.s[start..self.i]).replace("\\n", "\n");
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

/// One key event, down or up.
fn key_event(q: &mut Qmp, qcode: &str, down: bool) -> Result<(), String> {
    let ev = Value::obj([
        ("type", "key".into()),
        (
            "data",
            Value::obj([
                ("down", down.into()),
                ("key", Value::obj([("type", "qcode".into()), ("data", qcode.into())])),
            ]),
        ),
    ]);
    q.execute("input-send-event", Some(Value::obj([("events", Value::Arr(vec![ev]))]))).map(|_| ())
}

/// A key, or a chord (`"MetaLeft+KeyM"`): pressed in order, let go in reverse.
fn chord(q: &mut Qmp, spec: &str) -> Result<(), String> {
    let codes =
        spec.split('+').map(|c| qmp::qcode(c).ok_or_else(|| format!("no key {c}"))).collect::<Result<Vec<_>, _>>()?;
    for c in &codes {
        key_event(q, c, true)?;
    }
    for c in codes.iter().rev() {
        key_event(q, c, false)?;
    }
    Ok(())
}

/// The keys that type `c` on the US layout (shifted where it needs Shift).
fn keys_for(c: char) -> Option<String> {
    let plain = |k: &str| Some(k.to_string());
    let shifted = |k: &str| Some(format!("ShiftLeft+{k}"));
    match c {
        'a'..='z' => plain(&format!("Key{}", c.to_ascii_uppercase())),
        'A'..='Z' => shifted(&format!("Key{c}")),
        '0'..='9' => plain(&format!("Digit{c}")),
        ' ' => plain("Space"),
        '\n' => plain("Enter"),
        '-' => plain("Minus"),
        '_' => shifted("Minus"),
        '.' => plain("Period"),
        '/' => plain("Slash"),
        '=' => plain("Equal"),
        ',' => plain("Comma"),
        ';' => plain("Semicolon"),
        ':' => shifted("Semicolon"),
        _ => None,
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
                Step::Key(code) => chord(&mut q, code)?,
                Step::Type(text) => {
                    for c in text.chars() {
                        chord(&mut q, &keys_for(c).ok_or_else(|| format!("can't type {c:?}"))?)?;
                        // A typist's pace, on film.
                        record(&mut q, &frames, &mut next, 110)?;
                    }
                }
            }
        }
        Ok::<u32, String>(next)
    })();
    let _ = vm.kill();
    let _ = vm.wait();
    let n = result?;
    let mp4 = out.join(format!("{}.mp4", tour.name));
    // The firmware starts at a smaller mode: size the film to the last frame (the
    // OS's own mode) and fit earlier frames inside it, aspect kept.
    let last = std::fs::read(frames.join(format!("{:05}.ppm", n.saturating_sub(1)))).map_err(|e| e.to_string())?;
    let (w, h, _) = fimage::ppm::parse(&last)?;
    let mut fit = format!(
        "scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=0x{:06X}",
        fscreen::colours::BACKGROUND
    );
    if tour.stopwatch {
        // Seconds since power on (the first frame), to the millisecond.
        let font = util::repo_root().join("assets/fonts/noto/NotoSansMono-Regular.ttf");
        fit.push_str(&format!(
            ",drawtext=fontfile='{}':text='%{{eif\\:t\\:d}}.%{{eif\\:mod(t*1000\\,1000)\\:d\\:3}} s since power on':x=w-tw-32:y=32:fontsize=36:fontcolor=white:box=1:boxcolor=0x000000AA:boxborderw=12",
            font.display()
        ));
    }
    util::run_ok(
        Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-framerate", &FPS.to_string(), "-i"])
            .arg(frames.join("%05d.ppm"))
            .args(["-vf", &fit, "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "18"])
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
    fn parses_chords_typing_and_the_stopwatch() {
        let t =
            parse("Tour(name: \"t\", stopwatch: true, steps: [Key(code: \"MetaLeft+KeyM\"), Type(text: \"ps\\n\")])")
                .unwrap();
        assert!(t.stopwatch);
        assert_eq!(t.steps, vec![Step::Key("MetaLeft+KeyM".into()), Step::Type("ps\n".into())]);
        assert_eq!(keys_for('A').as_deref(), Some("ShiftLeft+KeyA"));
        assert_eq!(keys_for('\n').as_deref(), Some("Enter"));
        assert!(keys_for('€').is_none());
        // Every tour in the repo parses, and types only what it can.
        for e in std::fs::read_dir(util::repo_root().join("showcase/tours")).unwrap().flatten() {
            let t = parse(&std::fs::read_to_string(e.path()).unwrap()).unwrap();
            for s in &t.steps {
                if let Step::Type(text) = s {
                    assert!(text.chars().all(|c| keys_for(c).is_some()), "{}: {text:?}", t.name);
                }
            }
        }
    }

    #[test]
    fn refuses_unknown_steps() {
        assert!(parse("Tour(name: \"x\", steps: [Dance(ms: 1)])").is_err());
    }
}
