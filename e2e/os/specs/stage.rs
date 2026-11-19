// SPDX-License-Identifier: Apache-2.0
//! Stage (TASK-057): windows on the real screen, moved, focused and closed
//! with the real mouse and keyboard (PS/2, through drv-i8042), read back
//! from stage and from the screen's own pixels, in both themes.

use crate::Ctx;
use crate::art;
use fjson::Value;
use fui::tokens_gen::dark;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Win {
    pub(crate) id: u32,
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) w: i32,
    pub(crate) h: i32,
    pub(crate) focused: bool,
    pub(crate) z: u32,
}

fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    line.split_whitespace().find_map(|w| w.strip_prefix(name).and_then(|v| v.strip_prefix('=')))
}

/// Stage's windows, bottom to top, and its theme.
pub(crate) fn list(ctx: &Ctx) -> Result<(Vec<Win>, String), String> {
    let lines = ctx.door("stage list")?;
    let mut out = Vec::new();
    let mut theme = String::new();
    for l in &lines {
        if let Some(t) = l.strip_prefix("theme ") {
            theme = t.to_string();
            continue;
        }
        if l.starts_with("scale ") {
            continue;
        }
        let num = |n: &str| field(l, n).and_then(|v| v.parse::<i64>().ok()).ok_or(format!("no {n} in {l:?}"));
        out.push(Win {
            id: num("id")? as u32,
            x: num("x")? as i32,
            y: num("y")? as i32,
            w: num("w")? as i32,
            h: num("h")? as i32,
            focused: field(l, "focused") == Some("true"),
            z: num("z")? as u32,
        });
    }
    Ok((out, theme))
}

pub(crate) fn win(ws: &[Win], id: u32) -> Result<Win, String> {
    ws.iter().find(|w| w.id == id).cloned().ok_or(format!("window {id} isn't on screen: {ws:?}"))
}

fn open(ctx: &Ctx, w: u32, h: u32, rgb: u32, title: &str) -> Result<u32, String> {
    let lines = ctx.door(&format!("stage open {w} {h} {rgb:06x} {title}"))?;
    lines
        .iter()
        .find_map(|l| l.strip_prefix("window ").and_then(|v| v.parse().ok()))
        .ok_or(format!("stage open said {lines:?}"))
}

pub(crate) fn ok(ctx: &Ctx, cmd: &str) -> Result<(), String> {
    let lines = ctx.door(cmd)?;
    if lines != ["ok"] {
        return Err(format!("{cmd}: {lines:?}"));
    }
    Ok(())
}

fn pointer(ctx: &Ctx) -> Result<(i32, i32), String> {
    let lines = ctx.door("stage pointer")?;
    let w: Vec<i32> = lines
        .first()
        .and_then(|l| l.strip_prefix("pointer "))
        .map(|v| v.split_whitespace().filter_map(|n| n.parse().ok()).collect())
        .unwrap_or_default();
    if w.len() != 3 {
        return Err(format!("stage pointer said {lines:?}"));
    }
    Ok((w[0], w[1]))
}

fn mouse(ctx: &mut Ctx, params: Vec<(&str, Value)>) -> Result<(), String> {
    let params = Value::Obj(params.into_iter().map(|(k, v)| (k.to_string(), v)).collect());
    ctx.cdp.call("Input.dispatchMouseEvent", params).map(|_| ())
}

/// Moves the pointer to (x, y) with the real mouse, in steps, and waits
/// until stage has it there.
pub(crate) fn move_to(ctx: &mut Ctx, x: i32, y: i32) -> Result<(), String> {
    let (mut px, mut py) = pointer(ctx)?;
    let start = Instant::now();
    while (px, py) != (x, y) {
        let (dx, dy) = ((x - px).clamp(-100, 100), (y - py).clamp(-100, 100));
        mouse(
            ctx,
            vec![("type", "mouseMoved".into()), ("deltaX", Value::from(dx as f64)), ("deltaY", Value::from(dy as f64))],
        )?;
        std::thread::sleep(Duration::from_millis(60));
        (px, py) = pointer(ctx)?;
        if start.elapsed() > Duration::from_secs(20) {
            return Err(format!("the pointer stopped at ({px}, {py}), not ({x}, {y})"));
        }
    }
    Ok(())
}

pub(crate) fn button(ctx: &mut Ctx, down: bool) -> Result<(), String> {
    if down {
        ctx.note_press();
    }
    let kind = if down { "mousePressed" } else { "mouseReleased" };
    mouse(ctx, vec![("type", kind.into()), ("button", "left".into())])?;
    std::thread::sleep(Duration::from_millis(100));
    Ok(())
}

/// One notch of the wheel, up (away from the user) or down.
pub(crate) fn wheel(ctx: &mut Ctx, up: bool) -> Result<(), String> {
    let dy = if up { -120.0 } else { 120.0 };
    mouse(ctx, vec![("type", "mouseWheel".into()), ("deltaX", Value::from(0.0)), ("deltaY", Value::from(dy))])?;
    std::thread::sleep(Duration::from_millis(100));
    Ok(())
}

pub(crate) fn key(ctx: &mut Ctx, code: &str, down: bool) -> Result<(), String> {
    if down {
        ctx.note_key();
    }
    let kind = if down { "keyDown" } else { "keyUp" };
    ctx.cdp.call("Input.dispatchKeyEvent", Value::obj([("type", kind.into()), ("code", code.into())]))?;
    std::thread::sleep(Duration::from_millis(60));
    Ok(())
}

/// Alt held, `code` pressed and let go, Alt let go.
fn alt(ctx: &mut Ctx, code: &str) -> Result<(), String> {
    key(ctx, "AltLeft", true)?;
    key(ctx, code, true)?;
    key(ctx, code, false)?;
    key(ctx, "AltLeft", false)
}

/// Waits up to 10 s for stage's windows to satisfy `want`.
pub(crate) fn until(ctx: &Ctx, what: &str, want: impl Fn(&[Win]) -> bool) -> Result<Vec<Win>, String> {
    let start = Instant::now();
    loop {
        let (ws, _) = list(ctx)?;
        if want(&ws) {
            return Ok(ws);
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err(format!("{what}: stage has {ws:?}"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Waits up to 10 s for every (x, y) on the screen to be its colour.
pub(crate) fn expect_pixels(ctx: &mut Ctx, want: &[(i32, i32, u32, &str)]) -> Result<(), String> {
    let start = Instant::now();
    loop {
        let shot = ctx.screenshot()?;
        let at = |x: i32, y: i32| {
            let i = (y as usize * shot.w as usize + x as usize) * 3;
            (shot.pixels[i] as u32) << 16 | (shot.pixels[i + 1] as u32) << 8 | shot.pixels[i + 2] as u32
        };
        let wrong: Vec<String> = want
            .iter()
            .filter(|(x, y, rgb, _)| at(*x, *y) != *rgb)
            .map(|(x, y, rgb, what)| format!("{what} at ({x}, {y}) is {:06x}, not {rgb:06x}", at(*x, *y)))
            .collect();
        if wrong.is_empty() {
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err(wrong.join("; "));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn events(ctx: &Ctx) -> Result<Vec<String>, String> {
    ctx.door("stage events")
}

/// `@stage.windows`: two test windows open centred and cascaded, the newer
/// focused on top; each surface shows its client's pixels under a title
/// strip, the focused one's raised. Pressing the older one raises and
/// focuses it (both windows told); dragging its strip moves it; Alt+Tab
/// steps focus back; Alt+F4 and the close button ask the window to close
/// (the door's windows then close, as an app would); keys reach only the
/// focused window. Then the same in the light theme (TASK-057, FR-028).
pub fn windows(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("stage: first frame")?;
    ok(ctx, "stage theme dark")?;
    events(ctx)?;
    let a = open(ctx, 400, 300, 0x2050A0, "Alpha")?;
    let b = open(ctx, 300, 200, 0xA02050, "Beta")?;
    let (ws, theme) = list(ctx)?;
    let (wa, wb) = (win(&ws, a)?, win(&ws, b)?);
    if theme != "Dark" || !wb.focused || wa.focused || wb.z <= wa.z {
        return Err(format!("after two opens: {ws:?}, theme {theme}"));
    }
    if (wa.w, wa.h, wb.w, wb.h) != (400, 332, 300, 232) || (wb.x - wa.x, wb.y - wa.y) != (82, 82) {
        return Err(format!("not centred and cascaded: {wa:?} {wb:?}"));
    }
    // The surfaces, the focused strip raised, the other plain, the ground.
    expect_pixels(
        ctx,
        &[
            (wb.x + wb.w / 2, wb.y + 32 + wb.h / 2 - 16, 0xA02050, "Beta's surface"),
            (wa.x + 20, wa.y + 60, 0x2050A0, "Alpha's surface"),
            (
                wb.x + wb.w - 60,
                wb.y + 16,
                art::strip("dark", true, (wb.x, wb.y, wb.w), 32, wb.x + wb.w - 60, wb.y + 16),
                "Beta's focused strip",
            ),
            (
                wa.x + 200,
                wa.y + 16,
                art::strip("dark", false, (wa.x, wa.y, wa.w), 32, wa.x + 200, wa.y + 16),
                "Alpha's plain strip",
            ),
            (5, 5, art::ground("dark", 5, 5), "the ground"),
        ],
    )?;

    // Pressing Alpha's surface raises and focuses it; both are told.
    move_to(ctx, wa.x + 20, wa.y + 50)?;
    button(ctx, true)?;
    button(ctx, false)?;
    until(ctx, "Alpha raised and focused", |ws| {
        let (a, b) = (win(ws, a), win(ws, b));
        a.as_ref().is_ok_and(|a| a.focused) && b.as_ref().is_ok_and(|b| !b.focused) && a.unwrap().z > b.unwrap().z
    })?;
    let heard = events(ctx)?;
    for want in
        [format!("event {a} focus true"), format!("event {b} focus false"), format!("event {a} pointer Down 20 18 1")]
    {
        if !heard.contains(&want) {
            return Err(format!("the windows didn't hear {want:?}: {heard:?}"));
        }
    }

    // Dragging Alpha's strip moves it.
    move_to(ctx, wa.x + 200, wa.y + 16)?;
    button(ctx, true)?;
    move_to(ctx, wa.x + 260, wa.y + 56)?;
    button(ctx, false)?;
    let moved =
        win(&until(ctx, "Alpha moved", |ws| win(ws, a).is_ok_and(|w| (w.x, w.y) == (wa.x + 60, wa.y + 40)))?, a)?;
    expect_pixels(
        ctx,
        &[
            (moved.x + 20, moved.y + 60, 0x2050A0, "Alpha's surface where it went"),
            (wa.x + 10, wa.y + 10, art::ground("dark", wa.x + 10, wa.y + 10), "the ground where it was"),
        ],
    )?;

    // Alt+Tab: Beta again. Keys go to the focused window only.
    alt(ctx, "Tab")?;
    until(ctx, "Alt+Tab to Beta", |ws| win(ws, b).is_ok_and(|w| w.focused))?;
    events(ctx)?;
    key(ctx, "KeyA", true)?;
    key(ctx, "KeyA", false)?;
    let heard = events(ctx)?;
    if !heard.contains(&format!("event {b} key 0x04 down"))
        || heard.iter().any(|e| e.starts_with(&format!("event {a} key")))
    {
        return Err(format!("A typed with Beta focused: {heard:?}"));
    }

    // Alt+F4 asks Beta to close; the door closes it.
    alt(ctx, "F4")?;
    until(ctx, "Beta closed by Alt+F4", |ws| win(ws, b).is_err() && win(ws, a).is_ok_and(|w| w.focused))?;
    if !events(ctx)?.contains(&format!("event {b} close")) {
        return Err("Beta wasn't asked to close".into());
    }

    // The close button asks Alpha.
    move_to(ctx, moved.x + moved.w - 16, moved.y + 16)?;
    button(ctx, true)?;
    button(ctx, false)?;
    until(ctx, "Alpha closed by its button", |ws| win(ws, a).is_err())?;
    let (gx, gy) = (moved.x + 20, moved.y + 60);
    expect_pixels(ctx, &[(gx, gy, art::ground("dark", gx, gy), "the ground where Alpha was")])?;

    // The light theme: the ground and the strip follow it.
    let c = open(ctx, 360, 240, 0x30A060, "Gamma")?;
    ok(ctx, "stage theme light")?;
    let wc = win(&list(ctx)?.0, c)?;
    expect_pixels(
        ctx,
        &[
            (5, 5, art::ground("light", 5, 5), "the light ground"),
            (
                wc.x + wc.w - 60,
                wc.y + 16,
                art::strip("light", true, (wc.x, wc.y, wc.w), 32, wc.x + wc.w - 60, wc.y + 16),
                "Gamma's light strip",
            ),
            (wc.x + 30, wc.y + 60, 0x30A060, "Gamma's surface"),
        ],
    )?;
    if list(ctx)?.1 != "Light" || !events(ctx)?.contains(&format!("event {c} theme Light")) {
        return Err("the theme didn't reach stage and the window".into());
    }
    ok(ctx, "stage theme dark")?;
    ok(ctx, &format!("stage close {c}"))?;
    until(ctx, "Gamma closed", |ws| win(ws, c).is_err())?;
    expect_pixels(ctx, &[(5, 5, art::ground("dark", 5, 5), "the dark ground again")])
}

/// The screen's pixel at (x, y), 0xRRGGBB.
pub(crate) fn pixel(ctx: &mut Ctx, x: i32, y: i32) -> Result<u32, String> {
    let shot = ctx.screenshot()?;
    let i = (y as usize * shot.w as usize + x as usize) * 3;
    Ok((shot.pixels[i] as u32) << 16 | (shot.pixels[i + 1] as u32) << 8 | shot.pixels[i + 2] as u32)
}

pub(crate) fn green(p: u32) -> bool {
    crate::greenish((p >> 16) as u8, (p >> 8) as u8, p as u8)
}

/// Waits up to 10 s for the pixels at (x + dx, y + dy) around the pointer at
/// (x, y) to be green (`true`) or not (`false`): the ring at each radius.
fn expect_ring(ctx: &mut Ctx, (x, y): (i32, i32), want: &[(i32, bool)], what: &str) -> Result<(), String> {
    let start = Instant::now();
    loop {
        let mut wrong = Vec::new();
        for &(r, on) in want {
            for (dx, dy) in [(r, 0), (-r, 0), (0, r), (0, -r)] {
                let p = pixel(ctx, x + dx, y + dy)?;
                if green(p) != on {
                    wrong.push(format!("{p:06x} at ({dx}, {dy}) is {}green", if on { "not " } else { "" }));
                }
            }
        }
        if wrong.is_empty() {
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err(format!("{what}: {}", wrong.join("; ")));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// `@ui.cursor`: the fleet cursor, drawn by stage from its first frame (the
/// boot specs see it at the centre): the ring (radius 17) round the dot,
/// following the real mouse; over the close button it grows (×1.45) with a
/// faint fill; held down it shrinks (×0.78), and comes back when let go; a
/// window that asks for the text cursor gets the I-beam, and the ring steps
/// aside (TASK-058, FR-029).
pub fn cursor(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("stage: first frame")?;
    ok(ctx, "stage theme dark")?;
    ok(ctx, "stage scale 100")?;
    let ground = |x: i32, y: i32| art::ground("dark", x, y);
    // On the ground: the ring at 16, nothing inside it but the dot.
    move_to(ctx, 300, 300)?;
    expect_ring(ctx, (300, 300), &[(16, true), (8, false), (22, false)], "the ring at rest")?;
    if !green(pixel(ctx, 300, 300)?) {
        return Err("no dot at the pointer".into());
    }
    // It follows the mouse; where it was is the ground again.
    move_to(ctx, 420, 300)?;
    expect_ring(ctx, (420, 300), &[(16, true), (8, false)], "the ring where it went")?;
    expect_pixels(
        ctx,
        &[(316, 300, ground(316, 300), "where the ring was"), (300, 300, ground(300, 300), "where the dot was")],
    )?;

    // Over the close button: bigger, with a faint fill.
    let w = open(ctx, 360, 240, 0x2050A0, "Cursor")?;
    let f = win(&list(ctx)?.0, w)?;
    let close = (f.x + f.w - 16, f.y + 16);
    move_to(ctx, close.0, close.1)?;
    expect_ring(ctx, close, &[(24, true), (16, false)], "the ring over the close button")?;
    let strip = art::strip("dark", true, (f.x, f.y, f.w), 32, close.0 - 10, close.1);
    let filled = pixel(ctx, close.0 - 10, close.1)?;
    if filled == strip || green(filled) {
        return Err(format!("no faint fill over the close button: {filled:06x}"));
    }

    // Held down on the surface: smaller, then back.
    let mid = (f.x + f.w / 2, f.y + 32 + (f.h - 32) / 2);
    move_to(ctx, mid.0, mid.1)?;
    expect_ring(ctx, mid, &[(16, true)], "the ring over the surface")?;
    button(ctx, true)?;
    expect_ring(ctx, mid, &[(12, true), (16, false)], "the ring held down")?;
    button(ctx, false)?;
    expect_ring(ctx, mid, &[(16, true), (12, false)], "the ring let go")?;

    // The text cursor: an I-beam, and no ring.
    ok(ctx, &format!("stage cursor {w} 2"))?;
    move_to(ctx, mid.0 + 1, mid.1)?;
    let at = (mid.0 + 1, mid.1);
    expect_pixels(ctx, &[(at.0, at.1 - 5, dark::COLOR_TEXT.xrgb(), "the I-beam's stem")])?;
    expect_ring(ctx, at, &[(16, false)], "no ring with the I-beam")?;
    ok(ctx, &format!("stage close {w}"))?;
    until(ctx, "the cursor's window closed", |ws| win(ws, w).is_err())?;
    Ok(())
}

/// `@stage.doors`: screenshots and injected input are the Brain's and
/// cdpd's only (the test door is refused both); a published accessibility
/// tree is kept with its window; the UI scale reaches stage and every
/// window, the title strip growing with it, and a scale off the 25 % steps
/// is refused (TASK-058, FR-028, FR-031).
pub fn doors(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("stage: first frame")?;
    for (cmd, refusal) in [
        ("stage shot", "only the Brain and cdpd take screenshots"),
        ("stage inject", "only the Brain (dev) and cdpd (e2e) inject input"),
    ] {
        let said = ctx.door(cmd)?.join(" ");
        if !said.contains(refusal) {
            return Err(format!("{cmd} wasn't refused: {said}"));
        }
    }
    let w = open(ctx, 300, 200, 0x804020, "Doors")?;
    ok(ctx, &format!("stage ax {w} 1234"))?;
    let lines = ctx.door("stage list")?;
    if !lines.iter().any(|l| l.contains(&format!("id={w} ")) && l.contains(" ax=1234 ")) {
        return Err(format!("the accessibility tree wasn't kept: {lines:?}"));
    }
    events(ctx)?;
    ok(ctx, "stage scale 150")?;
    let f = win(&until(ctx, "the strip at 150 %", |ws| win(ws, w).is_ok_and(|x| x.h == 200 + 48))?, w)?;
    if !ctx.door("stage list")?.iter().any(|l| l == "scale 150") {
        return Err("stage doesn't say 150 %".into());
    }
    if !events(ctx)?.contains(&format!("event {w} scale 150")) {
        return Err("the window wasn't told the scale".into());
    }
    let (sx, sy) = (f.x + f.w - 80, f.y + 40);
    expect_pixels(ctx, &[(sx, sy, art::strip("dark", true, (f.x, f.y, f.w), 48, sx, sy), "the 48 px strip")])?;
    let bad = ctx.door("stage scale 110")?.join(" ");
    if !bad.contains("steps of 25") {
        return Err(format!("110 % wasn't refused: {bad}"));
    }
    ok(ctx, "stage scale 100")?;
    until(ctx, "the strip back at 100 %", |ws| win(ws, w).is_ok_and(|x| x.h == 232))?;
    ok(ctx, &format!("stage close {w}"))?;
    until(ctx, "the doors' window closed", |ws| win(ws, w).is_err())?;
    Ok(())
}
