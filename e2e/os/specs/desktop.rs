// SPDX-License-Identifier: Apache-2.0
//! The desktop (TASK-059): the Rack and the launcher on the real screen,
//! worked with the real mouse and keyboard.

use crate::Ctx;
use crate::stage::{button, key, move_to};
use fui::tokens_gen::dark;
use std::time::{Duration, Instant};

const RACK_H: i32 = 56;

/// The raw `stage list` lines.
fn windows(ctx: &Ctx) -> Result<Vec<String>, String> {
    ctx.door("stage list")
}

/// Waits up to 10 s for stage to have (or not have) a window titled `title`.
fn expect_window(ctx: &Ctx, title: &str, there: bool) -> Result<String, String> {
    let start = Instant::now();
    loop {
        let lines = windows(ctx)?;
        let found = lines.iter().find(|l| l.ends_with(&format!("title={title}"))).cloned();
        if found.is_some() == there {
            return Ok(found.unwrap_or_default());
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err(format!("{title} {}: {lines:?}", if there { "never opened" } else { "never closed" }));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn luma(p: u32) -> u32 {
    ((p >> 16 & 0xFF) * 299 + (p >> 8 & 0xFF) * 587 + (p & 0xFF) * 114) / 1000
}

/// Waits for the log to say `text` more than `before` times.
fn expect_more(ctx: &Ctx, text: &str, before: usize) -> Result<(), String> {
    let start = Instant::now();
    while ctx.log_count(text) <= before {
        if start.elapsed() > Duration::from_secs(10) {
            return Err(format!("COM1 never said {text:?} again"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// `@desktop.rack`: the Rack along the screen's foot, stamped S4, over every
/// window. Three keys (the launcher, Terminal, Monitor) from the hardware
/// set, all lit (both apps are in the image); the 12-hour clock in its well. A key's tooltip
/// after 600 ms, gone when the pointer leaves; the launcher key opens the
/// launcher (TASK-059, FR-030).
pub fn rack(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("desktop: the Rack is up, 3 keys, ")?;
    ctx.expect_log(" S4=")?;
    let line = ctx.log_line_with("desktop: the Rack is up").unwrap_or_default();
    let clock = line.rsplit(", ").next().unwrap_or_default();
    let ok = clock.ends_with(" AM") || clock.ends_with(" PM");
    let (h, m) = clock.trim_end_matches(" AM").trim_end_matches(" PM").split_once(':').unwrap_or(("", ""));
    if !ok || !(1..=12).contains(&h.parse::<u32>().unwrap_or(0)) || m.len() != 2 || m.parse::<u32>().unwrap_or(99) > 59
    {
        return Err(format!("the clock says {clock:?}, not 12-hour h:mm AM/PM"));
    }
    // The faceplate along the foot (a desktop restarted by an earlier spec
    // draws it again within moments).
    let start = std::time::Instant::now();
    let shot = loop {
        let shot = ctx.screenshot()?;
        let (w, h) = (shot.w as usize, shot.h as usize);
        let px = |x: usize, y: usize| {
            let i = (y * w + x) * 3;
            (shot.pixels[i] as u32) << 16 | (shot.pixels[i + 1] as u32) << 8 | shot.pixels[i + 2] as u32
        };
        let top = h - RACK_H as usize;
        let plate = |y: i32| crate::art::rack("dark", (w as u32, RACK_H as u32), (w / 2) as i32, y);
        // Above where a line of text could sit.
        if px(w / 2, top) == plate(0) && px(w / 2, top + 6) == plate(6) {
            break shot;
        }
        if start.elapsed() > std::time::Duration::from_secs(5) {
            return Err("no Rack faceplate along the foot".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    let (sw, sh) = (shot.w as i32, shot.h as i32);
    let top = sh - RACK_H;
    let at = |x: i32, y: i32| {
        let i = (y as usize * shot.w as usize + x as usize) * 3;
        (shot.pixels[i] as u32) << 16 | (shot.pixels[i + 1] as u32) << 8 | shot.pixels[i + 2] as u32
    };
    // Keys at x 8, 56, 104 (40 px): each face as bright as the launcher's (an
    // app missing from the image would fade toward the faceplate).
    let face = |x0: i32| (x0 + 6..x0 + 34).map(|x| luma(at(x, top + 3 + 34))).max().unwrap_or(0);
    let (launcher, terminal, monitor) = (face(8), face(56), face(104));
    let plate = luma(crate::art::rack("dark", (sw as u32, RACK_H as u32), 28, 50));
    if launcher <= plate + 8 || terminal + 4 < launcher || monitor + 4 < launcher {
        return Err(format!(
            "keys: launcher {launcher}, Terminal {terminal}, Monitor {monitor}, the faceplate {plate}"
        ));
    }
    // The clock: green digits in a well at the right.
    let digits = (sw - 200..sw - 24)
        .flat_map(|x| (top + 8..sh - 8).map(move |y| (x, y)))
        .filter(|&(x, y)| crate::stage::green(at(x, y)))
        .count();
    if digits < 40 {
        return Err(format!("no clock digits at the Rack's right ({digits} green pixels)"));
    }

    // This setup has no disk: a guest session, and the rack says nothing is kept.
    let n = crate::sweep::until_q(ctx, &crate::session::sel("rack-no-disk"))?;
    let said = crate::sweep::attr(ctx, n, "aria-label")?.unwrap_or_default();
    if !said.starts_with("Nothing you make here is kept") {
        return Err(format!("the rack's guest notice says {said:?}"));
    }

    ctx.keep_shot("desktop")?;
    // Hover Terminal: its tooltip after 600 ms; gone when the pointer leaves.
    move_to(ctx, 56 + 20, top + 3 + 20)?;
    let tip = expect_window(ctx, "Tooltip", true)?;
    if !tip.contains("kind=Tooltip") || !tip.contains("focused=false") {
        return Err(format!("the tooltip isn't an unfocused tooltip: {tip}"));
    }
    move_to(ctx, sw / 2, sh / 2)?;
    expect_window(ctx, "Tooltip", false)?;
    // A window as tall as the screen opens: the Rack stays over it (its
    // faceplate still drawn), the tooltip still up under the still pointer.
    move_to(ctx, 56 + 20, top + 3 + 20)?;
    expect_window(ctx, "Tooltip", true)?;
    let tall = ctx.door("stage open 1280 768 303030 Tall")?;
    let tall =
        tall.iter().find_map(|l| l.strip_prefix("window ")).ok_or(format!("stage open said {tall:?}"))?.to_string();
    expect_window(ctx, "Tall", true)?;
    let shot = ctx.screenshot()?;
    let i = ((top as usize) * shot.w as usize + shot.w as usize / 2) * 3;
    let edge = (shot.pixels[i] as u32) << 16 | (shot.pixels[i + 1] as u32) << 8 | shot.pixels[i + 2] as u32;
    if edge != crate::art::rack("dark", (shot.w, RACK_H as u32), (shot.w / 2) as i32, 0) {
        return Err(format!("a tall window covered the Rack: {edge:06x} along its top edge"));
    }
    move_to(ctx, 56 + 21, top + 3 + 20)?;
    expect_window(ctx, "Tooltip", true)?;
    // A popup opening over the Rack under a still pointer: the Rack hears
    // the pointer leave (still over the key's place), and the tooltip goes.
    let cover = ctx.door(&format!("stage popup 0 {} 1280 {RACK_H} 303030 Cover", top))?;
    let id =
        cover.iter().find_map(|l| l.strip_prefix("window ")).ok_or(format!("stage popup said {cover:?}"))?.to_string();
    move_to(ctx, 56 + 20, top + 3 + 20)?;
    expect_window(ctx, "Tooltip", false)?;
    crate::stage::ok(ctx, &format!("stage close {id}"))?;
    crate::stage::ok(ctx, &format!("stage close {tall}"))?;
    expect_window(ctx, "Cover", false)?;
    expect_window(ctx, "Tall", false)?;
    // The launcher key: its tooltip, then a press opens the launcher.
    let opened = ctx.log_count("desktop: the launcher is open");
    move_to(ctx, 8 + 20, top + 3 + 20)?;
    expect_window(ctx, "Tooltip", true)?;
    button(ctx, true)?;
    button(ctx, false)?;
    expect_more(ctx, "desktop: the launcher is open", opened)?;
    expect_window(ctx, "Launcher", true)?;
    expect_window(ctx, "Tooltip", false)?;
    key(ctx, "Escape", true)?;
    key(ctx, "Escape", false)?;
    expect_window(ctx, "Launcher", false)?;
    move_to(ctx, sw / 2, sh / 2)?;
    Ok(())
}

/// `@desktop.launcher`: Super opens the launcher, centred 30 % from the top
/// and focused; typing fills its well ("No app called 'xyz'." when nothing
/// matches); Backspace takes it back; Esc closes it, and Super toggles it.
/// Super+T starts Terminal, its window opening (TASK-059, TASK-060).
pub fn launcher(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("desktop: the Rack is up")?;
    let opened = ctx.log_count("desktop: the launcher is open");
    key(ctx, "MetaLeft", true)?;
    key(ctx, "MetaLeft", false)?;
    expect_more(ctx, "desktop: the launcher is open", opened)?;
    let line = expect_window(ctx, "Launcher", true)?;
    let num = |name: &str| {
        line.split_whitespace()
            .find_map(|w| w.strip_prefix(name).and_then(|v| v.strip_prefix('=')))
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(-1)
    };
    let (x, y, w) = (num("x"), num("y"), num("w"));
    let shot = ctx.screenshot()?;
    if (x + w / 2 - shot.w as i32 / 2).abs() > 1 || (y - shot.h as i32 * 3 / 10).abs() > 1 || w != 560 {
        return Err(format!("the launcher isn't 560 px wide, centred, 30 % down: {line}"));
    }
    if !line.contains("focused=true") {
        return Err(format!("the launcher doesn't have focus: {line}"));
    }
    // The OS's own mark (TASK-077A): the green F, drawn from the owner's master.
    let mark = crate::sweep::until_q(ctx, "IMG[data-testid=\"launcher-mark\"]")?;
    let (mx0, my0, mx1, my1) = crate::sweep::box_of(ctx, mark)?;
    let shot = ctx.screenshot()?;
    let (mut green, mut all) = (0, 0);
    for py in my0..my1 {
        for px in mx0..mx1 {
            let i = (py as usize * shot.w as usize + px as usize) * 3;
            let (r, g, b) = (shot.pixels[i] as i32, shot.pixels[i + 1] as i32, shot.pixels[i + 2] as i32);
            all += 1;
            if g > 120 && g > r + 40 && g > b + 40 {
                green += 1;
            }
        }
    }
    if mx1 - mx0 != 28 || my1 - my0 != 28 || green * 5 < all {
        return Err(format!("the launcher's mark: {}x{}, {green} of {all} pixels the F's green", mx1 - mx0, my1 - my0));
    }
    // Typing shows in the well, and "No app called 'xyz'." below it.
    let row = |ctx: &mut Ctx, ry: i32, x0: i32, x1: i32| -> Result<Vec<u32>, String> {
        let shot = ctx.screenshot()?;
        Ok((x0..x1)
            .map(|px| {
                let i = (ry as usize * shot.w as usize + px as usize) * 3;
                (shot.pixels[i] as u32) << 16 | (shot.pixels[i + 1] as u32) << 8 | shot.pixels[i + 2] as u32
            })
            .collect())
    };
    let well = |ctx: &mut Ctx| row(ctx, y + 36, x + 24, x + w - 24);
    let before = well(ctx)?;
    for k in ["KeyX", "KeyY", "KeyZ"] {
        key(ctx, k, true)?;
        key(ctx, k, false)?;
    }
    let start = Instant::now();
    while well(ctx)? == before {
        if start.elapsed() > Duration::from_secs(10) {
            return Err("typing didn't show in the launcher's well".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    ctx.keep_shot("launcher")?;
    let message = row(ctx, y + 82, x + 16, x + 300)?.into_iter().filter(|&p| p != dark::COLOR_SURFACE.xrgb()).count();
    if message < 8 {
        return Err("no \"No app called 'xyz'.\" under the well".into());
    }
    for _ in 0..3 {
        key(ctx, "Backspace", true)?;
        key(ctx, "Backspace", false)?;
    }
    // Esc closes it; Super opens and closes it again.
    key(ctx, "Escape", true)?;
    key(ctx, "Escape", false)?;
    expect_window(ctx, "Launcher", false)?;
    for want in [true, false] {
        key(ctx, "MetaLeft", true)?;
        key(ctx, "MetaLeft", false)?;
        expect_window(ctx, "Launcher", want)?;
    }
    // Typing filters: "ter" finds Terminal; Esc closes.
    key(ctx, "MetaLeft", true)?;
    key(ctx, "MetaLeft", false)?;
    expect_window(ctx, "Launcher", true)?;
    // Super+T starts Terminal (its window opens).
    key(ctx, "Escape", true)?;
    key(ctx, "Escape", false)?;
    expect_window(ctx, "Launcher", false)?;
    let started = ctx.log_count("desktop: started terminal");
    key(ctx, "MetaLeft", true)?;
    key(ctx, "KeyT", true)?;
    key(ctx, "KeyT", false)?;
    key(ctx, "MetaLeft", false)?;
    expect_more(ctx, "desktop: started terminal", started)?;
    expect_window(ctx, "Terminal", true)?;
    if windows(ctx)?.iter().any(|l| l.ends_with("title=Launcher")) {
        return Err("Super+T opened the launcher".into());
    }
    Ok(())
}

/// `@desktop.sweep`: every control the desktop shows, worked by its box:
/// each Rack key (the launcher's opens the launcher, Esc closes it; an app's
/// starts its app), the launcher's search (what's typed is its value, and
/// nothing matches "zz") and each of its rows (a click starts that app and
/// closes the launcher) (TASK-063, FR-040).
pub fn sweep(ctx: &mut Ctx) -> Result<(), String> {
    use crate::sweep::{click, q_all, sweep, type_keys, until_attr, until_gone, until_q};
    ctx.expect_log("desktop: the Rack is up")?;
    const RACK: &str = "WINDOW[data-owner=\"desktop\"][data-kind=\"Panel\"]";
    const SEARCH: &str = "[data-testid=\"launcher-search\"]";
    until_q(ctx, &format!("{RACK} BUTTON"))?;
    let started = |ctx: &Ctx, svc: &str| ctx.log_count(&format!("desktop: started {svc}"));
    let before: Vec<(String, usize)> =
        ["terminal", "monitor"].iter().map(|s| (s.to_string(), started(ctx, s))).collect();
    let keys = sweep(ctx, RACK, "BUTTON", |ctx, c| {
        if c.testid == "rack-launcher" {
            until_q(ctx, SEARCH)?;
            key(ctx, "Escape", true)?;
            key(ctx, "Escape", false)?;
            return until_gone(ctx, SEARCH);
        }
        let svc = c.testid.trim_start_matches("rack-");
        let was = before.iter().find(|(s, _)| s == svc).map_or(0, |(_, n)| *n);
        ctx.expect_log_count(&format!("desktop: started {svc}"), was + 1)
    })?;
    if keys != 3 {
        return Err(format!("{keys} Rack keys, not 3"));
    }
    // The launcher's rows: each opened afresh, its row clicked.
    let open = |ctx: &mut Ctx| -> Result<(), String> {
        key(ctx, "MetaLeft", true)?;
        key(ctx, "MetaLeft", false)?;
        until_attr(ctx, SEARCH, "data-focused", "true")
    };
    open(ctx)?;
    let rows: Vec<String> = {
        let ids = q_all(ctx, "LISTBOX OPTION")?;
        let mut v = Vec::new();
        for id in ids {
            v.push(crate::sweep::attr(ctx, id, "data-testid")?.ok_or("a row with no test id")?);
        }
        v
    };
    // Files needs storage: listed only where the image has it (GPT, e2e and dev).
    if !(4..=5).contains(&rows.len()) {
        return Err(format!("the launcher lists {rows:?}, not Terminal, Monitor, Settings, Calculator (and Files)"));
    }
    key(ctx, "Escape", true)?;
    key(ctx, "Escape", false)?;
    until_gone(ctx, SEARCH)?;
    for row in &rows {
        open(ctx)?;
        let svc = row.trim_start_matches("launcher-").to_string();
        let was = started(ctx, &svc);
        let id = until_q(ctx, &format!("[data-testid=\"{row}\"]"))?;
        click(ctx, id)?;
        ctx.expect_log_count(&format!("desktop: started {svc}"), was + 1)?;
        until_gone(ctx, SEARCH)?;
    }
    // The search: what's typed is its value; "zz" leaves no rows.
    open(ctx)?;
    type_keys(ctx, "zz")?;
    until_attr(ctx, SEARCH, "aria-valuenow", "zz")?;
    if !q_all(ctx, "LISTBOX OPTION")?.is_empty() {
        return Err("rows for \"zz\"".into());
    }
    key(ctx, "Escape", true)?;
    key(ctx, "Escape", false)?;
    until_gone(ctx, SEARCH)
}

/// Stage's `frames` read-back: (composed, shown) since it started.
fn frames(ctx: &Ctx) -> Result<(u64, u64), String> {
    let lines = ctx.door("stage frames")?;
    let l = lines.iter().find_map(|l| l.strip_prefix("frames ")).ok_or(format!("stage frames said {lines:?}"))?;
    let mut n = l.split(' ').map(|v| v.parse::<u64>().map_err(|_| format!("stage frames said {lines:?}")));
    Ok((n.next().ok_or("no count")??, n.next().ok_or("no count")??))
}

/// Whether the desktop's clock may have changed its minute while frames
/// were counted, from `t0` to `t1` (the host's time, ms since the epoch).
/// The guest's clock is the host's RTC read in whole seconds at boot, so its
/// minute turns up to a second after the host's (a little more as it
/// drifts): a host minute that turned up to 1.5 s before `t0` may turn in
/// the guest inside the window.
fn minute_may_turn(t0: u64, t1: u64) -> bool {
    t0.saturating_sub(1_500).div_ceil(60_000) <= t1 / 60_000
}

/// How many frames stage put on the screen over two seconds, and when (the
/// host's seconds into the minute, for the error that says it). A change of
/// minute redraws the clock, a real change: then it's measured again.
fn drawn_over_2s(ctx: &Ctx) -> Result<(u64, String), String> {
    let now = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    let mut drawn = (0, String::new());
    for _ in 0..3 {
        let (t0, (_, a)) = (now(), frames(ctx)?);
        std::thread::sleep(Duration::from_secs(2));
        let ((_, b), t1) = (frames(ctx)?, now());
        let at = |t: u64| (t % 60_000) as f64 / 1000.0;
        drawn = (b - a, format!("the host's second {:.1} to {:.1} of its minute", at(t0), at(t1)));
        if drawn.0 == 0 || !minute_may_turn(t0, t1) {
            return Ok(drawn);
        }
    }
    Ok(drawn)
}

/// `@desktop.still`: nothing moves while idle (the fleet rule of
/// 2026-11-22). The desktop at rest, the pointer still, puts no frame on the
/// screen in two seconds; Monitor, open and seen, does (its live figures,
/// once a second); covered by a window, it puts none again: nothing is drawn
/// out of sight (TASK-120B).
pub fn still(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("desktop: the Rack is up")?;
    move_to(ctx, 200, 200)?;
    // The ring catches up with the dot, then stops.
    std::thread::sleep(Duration::from_millis(800));
    let (idle, when) = drawn_over_2s(ctx)?;
    if idle != 0 {
        return Err(format!("the idle desktop put {idle} frames on the screen in 2 s ({when})"));
    }
    key(ctx, "MetaLeft", true)?;
    key(ctx, "KeyM", true)?;
    key(ctx, "KeyM", false)?;
    key(ctx, "MetaLeft", false)?;
    let mon = expect_window(ctx, "Monitor", true)?;
    std::thread::sleep(Duration::from_millis(1500));
    let (seen, _) = drawn_over_2s(ctx)?;
    if seen == 0 {
        return Err("Monitor, open and seen, drew nothing in 2 s: the count is stuck".into());
    }
    let cover = ctx.door("stage open 1280 800 303030 Cover")?;
    let cover = cover.iter().find_map(|l| l.strip_prefix("window ")).ok_or(format!("stage open said {cover:?}"))?;
    let cover = cover.to_string();
    std::thread::sleep(Duration::from_millis(800));
    let hidden = drawn_over_2s(ctx);
    let placed = windows(ctx)?;
    crate::stage::ok(ctx, &format!("stage close {cover}"))?;
    // Monitor goes, as it came, whatever was counted: the next specs sample
    // the ground (a red count left it open, and @terminal.vt red after it).
    let gone = (|| {
        move_to(ctx, 300, 300)?;
        key(ctx, "AltLeft", true)?;
        key(ctx, "F4", true)?;
        key(ctx, "F4", false)?;
        key(ctx, "AltLeft", false)?;
        expect_window(ctx, "Monitor", false).map(|_| ())
    })();
    let (hidden, when) = hidden?;
    if hidden != 0 {
        return Err(format!(
            "Monitor, covered, still put {hidden} frames on the screen in 2 s, {when} ({mon} under {:?})",
            placed.iter().find(|l| l.ends_with("title=Cover"))
        ));
    }
    gone
}

#[cfg(test)]
mod still_tests {
    use super::minute_may_turn;

    const M: u64 = 60_000;

    #[test]
    fn a_minute_the_guest_turns_late_is_measured_again() {
        // Well inside a minute: nothing turns.
        assert!(!minute_may_turn(100 * M + 10_000, 100 * M + 12_100));
        // The host's minute turns in the window.
        assert!(minute_may_turn(100 * M - 1_000, 100 * M + 1_100));
        // The host's turned 0.4 s before: the guest's, up to a second later, may turn inside.
        assert!(minute_may_turn(100 * M + 400, 100 * M + 2_500));
        // 1.6 s before: the guest's has turned too.
        assert!(!minute_may_turn(100 * M + 1_600, 100 * M + 3_700));
    }
}
