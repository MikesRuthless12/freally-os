// SPDX-License-Identifier: Apache-2.0
//! Freally Terminal and Freally Shell (TASK-060, TASK-061) on the real
//! screen: recorded escape sequences replayed and the screen they leave
//! compared pixel for pixel with the same sequences drawn on the host by the
//! same code; every built-in run in the shell.

use crate::Ctx;
use crate::stage::{key, move_to};
use app_terminal::grid::Grid;
use app_terminal::render::{self, Palette, View};
use ffont::atlas::Target;
use ffont::text::{Face, cell};
use fui::theme::{Mode, Theme};
use std::time::{Duration, Instant};

/// What `version` says in this e2e image.
static VERSION_LINE: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| format!("Freally OS {} (e2e)", env!("CARGO_PKG_VERSION")));

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Starts the terminal and waits (10 s) for it to answer; its screen lines.
pub(crate) fn open(ctx: &Ctx) -> Result<Vec<String>, String> {
    ctx.door("terminal open")?;
    let start = Instant::now();
    loop {
        let lines = ctx.door("terminal screen")?;
        if lines.iter().any(|l| l.starts_with("window ")) {
            return Ok(lines);
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err(format!("the terminal never answered: {lines:?}"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn feed(ctx: &Ctx, op: &str, bytes: &[u8]) -> Result<(), String> {
    let r = ctx.door(&format!("terminal {op} {}", hex(bytes)))?;
    if r != ["ok"] {
        return Err(format!("terminal {op}: {r:?}"));
    }
    Ok(())
}

/// The screen's rows as the terminal reads them back (row number → text).
fn rows(ctx: &Ctx) -> Result<Vec<(usize, String)>, String> {
    Ok(ctx
        .door("terminal screen")?
        .iter()
        .filter_map(|l| l.strip_prefix("row "))
        .filter_map(|l| l.split_once(' ').and_then(|(n, t)| Some((n.parse().ok()?, t.to_string()))))
        .collect())
}

fn screen_text(ctx: &Ctx) -> Result<String, String> {
    Ok(rows(ctx)?.into_iter().map(|(_, t)| t).collect::<Vec<_>>().join("\n"))
}

/// The terminal's window on screen: (x, y) of its frame.
fn frame(ctx: &Ctx) -> Result<(i32, i32), String> {
    let lines = ctx.door("stage list")?;
    let l = lines
        .iter()
        .find(|l| l.ends_with("owner=terminal title=Terminal") || l.contains("owner=terminal"))
        .ok_or(format!("no terminal window: {lines:?}"))?;
    let num = |n: &str| {
        l.split_whitespace()
            .find_map(|w| w.strip_prefix(n).and_then(|v| v.strip_prefix('=')))
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(-1)
    };
    Ok((num("x"), num("y")))
}

/// A recorded session: what a program writes, and the screen it should leave.
const SESSION: &[u8] = concat!(
    "\x1b[2J\x1b[H",
    "\x1b[1;32mfreally\x1b[0m:\x1b[34m~\x1b[0m$ ls --color\r\n",
    "\x1b[38;5;208morange\x1b[0m  \x1b[38;2;80;160;240mtruecolor\x1b[0m  \x1b[4munderlined\x1b[24m  \x1b[7minverse\x1b[27m\r\n",
    "\x1b[5;10Hplaced at 5,10\x1b[K",
    "\x1b[7;1H\x1b[41m   \x1b[42m   \x1b[43m   \x1b[44m   \x1b[45m   \x1b[46m   \x1b[0m",
    "\x1b[9;1Hwraps",
    "\x1b[10;94Hwrapping past the edge",
    "\x1b[12;24r\x1b[24;1Hlast\x1b[S\x1b[r",
    "\x1b]0;session\x07",
    "\x1b[?1049h\x1b[Halternate\x1b[?1049l",
    "\x1b[20;1H\x1b[1mbold\x1b[22m \x1b[2mdim\x1b[22m \x1b[9mstruck\x1b[29m \x1b[3mitalic\x1b[23m"
)
.as_bytes();

/// `@terminal.vt`: a recorded session (colours 16/256/truecolor, the
/// attributes, cursor placing, erasing, wrapping at the edge, a scroll
/// region, the title, the alternate screen coming and going) leaves the
/// screen the same sequences leave on the host, read back as text and
/// compared pixel for pixel; and typing on the real keyboard reaches the
/// shell (TASK-060, FR-032).
pub fn vt(ctx: &mut Ctx) -> Result<(), String> {
    open(ctx)?;
    move_to(ctx, 3, 3)?;
    feed(ctx, "feed", b"\x1bc")?;
    feed(ctx, "feed", SESSION)?;
    // The host's grid from the same bytes.
    let mut want = Grid::new(96, 28);
    want.feed(b"\x1bc");
    want.feed(SESSION);
    let got = rows(ctx)?;
    for y in 0..want.rows {
        let w = want.text(y);
        let g = got.iter().find(|(n, _)| *n == y).map(|(_, t)| t.clone()).unwrap_or_default();
        if w != g {
            return Err(format!("row {y} reads {g:?}, not {w:?}"));
        }
    }
    if !ctx.door("stage list")?.iter().any(|l| l.contains("title=session")) {
        return Err("the OSC title didn't reach the window's strip".into());
    }
    // Pixel for pixel: the grid area of the window against the host's drawing.
    let mut faces = [Face::new(include_bytes!("../../../assets/fonts/noto/NotoSansMono-Regular.ttf"))
        .map_err(|e| format!("{e:?}"))?];
    let c = cell(&mut faces[0], 14.0);
    let (w, h) = (96 * c.w as usize, 28 * c.h as usize);
    struct Canvas(usize, usize, Vec<u32>);
    impl Target for Canvas {
        fn size(&self) -> (usize, usize) {
            (self.0, self.1)
        }
        fn get(&self, x: usize, y: usize) -> u32 {
            self.2[y * self.0 + x]
        }
        fn set(&mut self, x: usize, y: usize, rgb: u32) {
            self.2[y * self.0 + x] = rgb;
        }
    }
    let p = Palette::of(&Theme::new(Mode::Dark, 1.0));
    let mut canvas = Canvas(w, h, vec![p.bg; w * h]);
    render::render(
        &mut canvas,
        &mut faces,
        14.0,
        c,
        (0, 0),
        &View { grid: &want, back: 0, selection: None, focused: true },
        &p,
    );
    let (fx, fy) = frame(ctx)?;
    let (ox, oy) = (fx + 8, fy + 32 + 8);
    let start = Instant::now();
    loop {
        let shot = ctx.screenshot()?;
        let mut diff = 0;
        for y in 0..h {
            for x in 0..w {
                let i = ((oy as usize + y) * shot.w as usize + ox as usize + x) * 3;
                let s = (shot.pixels[i] as u32) << 16 | (shot.pixels[i + 1] as u32) << 8 | shot.pixels[i + 2] as u32;
                if s != canvas.2[y * w + x] {
                    diff += 1;
                }
            }
        }
        if diff == 0 {
            break;
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err(format!("{diff} of {} pixels of the grid differ from the host's drawing", w * h));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    ctx.keep_shot("terminal-vt")?;
    // The real keyboard: "version", Enter.
    feed(ctx, "feed", b"\x1bc")?;
    feed(ctx, "type", b"\r")?;
    for k in ["KeyV", "KeyE", "KeyR", "KeyS", "KeyI", "KeyO", "KeyN", "Enter"] {
        key(ctx, k, true)?;
        key(ctx, k, false)?;
    }
    let start = Instant::now();
    while !screen_text(ctx)?.contains(VERSION_LINE.as_str()) {
        if start.elapsed() > Duration::from_secs(10) {
            return Err(format!("typing `version` on the keyboard gave: {}", screen_text(ctx)?));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

/// Runs a command in the shell on a cleared screen; the screen after.
fn shell(ctx: &Ctx, line: &str) -> Result<String, String> {
    feed(ctx, "type", b"clear\r")?;
    feed(ctx, "type", format!("{line}\r").as_bytes())?;
    std::thread::sleep(Duration::from_millis(100));
    screen_text(ctx)
}

pub(crate) fn expect(ctx: &Ctx, line: &str, want: &[&str]) -> Result<String, String> {
    let start = Instant::now();
    loop {
        let s = shell(ctx, line)?;
        if want.iter().all(|w| s.contains(w)) {
            return Ok(s);
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err(format!("`{line}` printed:\n{s}\n(wanted {want:?})"));
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// `@shell.builtins`: every built-in runs in the terminal and says what it
/// should, each answers `--help`, mistakes say how, `restart` isn't in an
/// e2e image, `theme` and `scale` reach stage (TASK-061, FR-033).
pub fn builtins(ctx: &mut Ctx) -> Result<(), String> {
    open(ctx)?;
    expect(ctx, "help", &["Commands", "log [service] [-f]", "theme dark|light"])?;
    expect(ctx, "version", &[VERSION_LINE.as_str()])?;
    expect(ctx, "ps", &["SERVICE", "STATE", "stage", "Running", "terminal"])?;
    expect(ctx, "boot", &["B0", "S4", "B0 to S4:"])?;
    expect(ctx, "mem", &["MiB used of", "MiB ("])?;
    expect(ctx, "uptime", &["up "])?;
    expect(ctx, "faults", &["Nothing has crashed since Freally OS started."])?;
    // `log stage`: stage's records only (its newest fill the screen).
    let log = expect(ctx, "log stage", &["stage        info"])?;
    let records: Vec<&str> =
        log.lines().filter(|l| l.trim_start().chars().next().is_some_and(|c| c.is_ascii_digit())).collect();
    if records.is_empty() || records.iter().any(|l| l.split_whitespace().nth(1) != Some("stage")) {
        return Err(format!("`log stage` showed other services' lines:\n{log}"));
    }
    let s = expect(ctx, "clear", &["freally:~$"])?;
    if s.lines().count() != 1 {
        return Err(format!("clear left more than the prompt:\n{s}"));
    }
    for name in ["ps", "boot", "log", "mem", "uptime", "clear", "theme", "scale", "reboot", "faults", "version", "help"]
    {
        expect(ctx, &format!("{name} --help"), &[name])?;
    }
    expect(ctx, "scale 110", &["steps of 25"])?;
    expect(ctx, "frob", &["no such command"])?;
    expect(ctx, "restart logger", &["no such command"])?;
    expect(ctx, "reboot", &["not yet"])?;
    expect(ctx, "theme light", &[])?;
    let start = Instant::now();
    while !ctx.door("stage list")?.iter().any(|l| l == "theme Light") {
        if start.elapsed() > Duration::from_secs(5) {
            return Err("`theme light` didn't reach stage".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    expect(ctx, "theme dark", &[])?;
    expect(ctx, "scale 125", &[])?;
    let start = Instant::now();
    while !ctx.door("stage list")?.iter().any(|l| l == "scale 125") {
        if start.elapsed() > Duration::from_secs(5) {
            return Err("`scale 125` didn't reach stage".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    expect(ctx, "scale 100", &[])?;
    // Line editing: "xversin", Left, "o", Home, Delete makes "version".
    feed(ctx, "type", b"clear\r")?;
    feed(ctx, "type", b"xversin\x1b[Do\x1b[H\x1b[3~\r")?;
    let start = Instant::now();
    while !screen_text(ctx)?.contains(VERSION_LINE.as_str()) {
        if start.elapsed() > Duration::from_secs(5) {
            return Err(format!("editing didn't make `version`: {}", screen_text(ctx)?));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // History: after `clear`, Up twice is `version` again.
    feed(ctx, "type", b"clear\r\x1b[A\x1b[A\r")?;
    let start = Instant::now();
    while !screen_text(ctx)?.contains(VERSION_LINE.as_str()) {
        if start.elapsed() > Duration::from_secs(5) {
            return Err(format!("Up didn't bring the line back: {}", screen_text(ctx)?));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    ctx.keep_shot("shell")?;
    Ok(())
}

/// Runs a command that changes something once (`expect` would run it again).
fn once(ctx: &Ctx, line: &str, want: &[&str]) -> Result<(), String> {
    let s = shell(ctx, line)?;
    if !want.iter().all(|w| s.contains(w)) {
        return Err(format!("`{line}` printed:\n{s}\n(wanted {want:?})"));
    }
    Ok(())
}

fn session(ctx: &Ctx, user: &str) -> Result<(), String> {
    let said = ctx.door(&format!("vfs session {user}"))?;
    if said != ["ok"] {
        return Err(format!("vfs session {user}: {said:?}"));
    }
    Ok(())
}

/// `@fs.files` (TASK-100): in the session's home, through the Terminal,
/// fstd::fs, vfs, ffs and drv-nvme: make a folder, write a file, read it,
/// move it, list, remove; `/` and `/system` as they should be.
pub fn fs_files(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("vfs: / with /system and /home")?;
    session(ctx, "tester")?;
    open(ctx)?;
    expect(ctx, "whoami", &["tester"])?;
    expect(ctx, "ls /", &["home/", "system/"])?;
    expect(ctx, "ls /system", &["accounts/"])?;
    once(ctx, "mkdir /home/tester/Docs", &[])?;
    expect(ctx, "ls /home/tester", &["Docs/"])?;
    once(ctx, "write /home/tester/Docs/note.txt hello from freally", &[])?;
    expect(ctx, "cat /home/tester/Docs/note.txt", &["hello from freally"])?;
    once(ctx, "mv /home/tester/Docs/note.txt /home/tester/Docs/kept.txt", &[])?;
    expect(ctx, "ls /home/tester/Docs", &["kept.txt"])?;
    expect(ctx, "cat /home/tester/Docs/note.txt", &["NotFound"])?;
    once(ctx, "rm /home/tester/Docs/kept.txt", &[])?;
    expect(ctx, "ls /home/tester/Docs", &["(empty)"])?;
    once(ctx, "rm /home/tester/Docs", &[])?;
    expect(ctx, "ls /home/tester", &["(empty)"])?;
    Ok(())
}

/// `@fs.permissions` (TASK-100): another user's home is `AccessDenied`,
/// and isn't listed; `/system` is read-only; with no session, no home.
pub fn fs_permissions(ctx: &mut Ctx) -> Result<(), String> {
    session(ctx, "sam")?;
    open(ctx)?;
    once(ctx, "write /home/sam/secret.txt only sam reads this", &[])?;
    expect(ctx, "cat /home/sam/secret.txt", &["only sam reads this"])?;
    session(ctx, "tester")?;
    expect(ctx, "cat /home/sam/secret.txt", &["AccessDenied"])?;
    expect(ctx, "ls /home/sam", &["AccessDenied"])?;
    let homes = expect(ctx, "ls /home", &["tester/"])?;
    if homes.contains("sam") {
        return Err(format!("/home lists another account's home:\n{homes}"));
    }
    expect(ctx, "write /system/x hi", &["AccessDenied"])?;
    expect(ctx, "write /home/sam/x hi", &["AccessDenied"])?;
    session(ctx, "")?;
    expect(ctx, "whoami", &["No one"])?;
    expect(ctx, "ls /home/tester", &["AccessDenied"])?;
    Ok(())
}

/// `@fs.persists` (TASK-100): a file written and synced is there after the
/// machine resets mid-session; the mount after it runs ffsck, clean.
pub fn fs_persists(ctx: &mut Ctx) -> Result<(), String> {
    session(ctx, "keeper")?;
    open(ctx)?;
    once(ctx, "write /home/keeper/kept.txt survives a reset", &[])?;
    let mounted = ctx.log_count("ffs: generation");
    ctx.reset()?;
    let start = Instant::now();
    while ctx.log_count("ffs: generation") <= mounted || ctx.log_count("vfs: / with /system and /home") < 2 {
        if start.elapsed() > Duration::from_secs(60) {
            return Err("after the reset, ffs never said it mounted an unclean generation".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    ctx.expect_log("ffs: ffsck: clean")?;
    session(ctx, "keeper")?;
    open(ctx)?;
    expect(ctx, "cat /home/keeper/kept.txt", &["survives a reset"])?;
    Ok(())
}

/// A process's memory, as conductor counts it.
fn mem_of(ctx: &mut Ctx, name: &str) -> Result<i64, String> {
    let r = ctx.cdp.call("Freally.getProcesses", fjson::Value::Obj(Vec::new()))?;
    r.get("processes")
        .and_then(fjson::Value::as_array)
        .unwrap_or_default()
        .iter()
        .find(|p| p.get("name").and_then(fjson::Value::as_str) == Some(name))
        .and_then(|p| p.get("memBytes").and_then(fjson::Value::as_i64))
        .ok_or_else(|| format!("no memory for {name}"))
}

/// `@memory.steady`: memory a program frees is given back. Forty `ps` in
/// the Terminal, each a call to conductor whose reply (over 4 KiB) conductor
/// builds and frees, leave conductor's memory where it was, within 64 KiB:
/// until 2026-11-19 every block over 2 KiB a program freed stayed mapped, so
/// every call leaked on both ends. (The Terminal's own memory grows by its
/// scrollback, so it isn't the measure.)
pub fn memory_steady(ctx: &mut Ctx) -> Result<(), String> {
    open(ctx)?;
    for _ in 0..5 {
        expect(ctx, "ps", &["SERVICE"])?;
    }
    let before = mem_of(ctx, "conductor")?;
    for _ in 0..40 {
        feed(ctx, "type", b"ps\r")?;
    }
    expect(ctx, "ps", &["SERVICE"])?;
    let after = mem_of(ctx, "conductor")?;
    ctx.note(&format!("conductor's memory: {} KiB, then {} KiB after 41 calls", before / 1024, after / 1024));
    if after - before > 64 * 1024 {
        return Err(format!(
            "41 calls left conductor {} KiB bigger: freed memory isn't given back",
            (after - before) / 1024
        ));
    }
    Ok(())
}
