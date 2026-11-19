// SPDX-License-Identifier: Apache-2.0
//! The service specs (`@services.*`, `@log.*`), driven through conductor's
//! e2e test door on COM3 until cdpd exists (Phase 3).

use crate::{Ctx, Expect};

/// `@services.restart`: every service that restarts is killed in turn, and
/// conductor has it back (started and registered again) within 200 ms
/// (TASK-038).
pub fn restart(ctx: &mut Ctx) -> Result<(), String> {
    ctx.allow_restarts();
    ctx.expect_log("[logger] logger: ready")?;
    let listed = ctx.door("services")?;
    let restartable: Vec<String> = listed
        .iter()
        .filter_map(|l| l.strip_prefix("svc "))
        .filter_map(|l| l.split_once(' '))
        .filter(|(_, state)| *state == "Running")
        .map(|(n, _)| n.to_string())
        .collect();
    if restartable.is_empty() {
        return Err(format!("no running service to kill: {listed:?}"));
    }
    // The logger last: the services it outlives must still log afterwards (below).
    let mut restartable = restartable;
    restartable.sort_by_key(|n| n == "logger");
    for name in &restartable {
        let answer = ctx.door(&format!("kill {name}"))?;
        if answer.first().map(String::as_str) != Some("ok") {
            return Err(format!("kill {name}: {answer:?}"));
        }
        let back = answer
            .iter()
            .find_map(|l| l.strip_prefix(&format!("back {name} ")))
            .ok_or(format!("{name} never came back: {answer:?}"))?;
        let ms: u64 = back.parse().map_err(|_| format!("an unreadable time: {back:?}"))?;
        if ms > 200 {
            return Err(format!("{name} took {ms} ms to come back (at most 200)"));
        }
    }
    // A service that lived through the logger's restart still logs: drv-i8042
    // (restarted before the logger was) reports a key press (e2e images log keys).
    if restartable.iter().any(|n| n == "drv-i8042") && restartable.last().is_some_and(|n| n == "logger") {
        let before = ctx.log_count("drv-i8042: key 0x14 down");
        key(ctx, "KeyQ", true)?;
        key(ctx, "KeyQ", false)?;
        ctx.expect_log_count("drv-i8042: key 0x14 down", before + 1)
            .map_err(|e| format!("a service that outlived the logger's restart stopped logging: {e}"))?;
    }
    Ok(())
}

/// `@log.read`: the logger keeps every service's lines and the kernel's own
/// (drained from its early ring), mirrors services' lines to COM1, and `read`
/// pages them back (TASK-039).
pub fn read(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("[logger] logger: ready (mirroring to COM1)")?;
    ctx.expect_log("[conductor] conductor: logger ready")?;
    let recs = ctx.door("log 200")?;
    for want in [
        "rec kernel freally-core: conductor started (C0 ",
        "rec logger logger: ready (mirroring to COM1)",
        "rec conductor conductor: logger ready",
    ] {
        if !recs.iter().any(|r| r.starts_with(want)) {
            return Err(format!("the log has no {want:?} (it has {} records)", recs.len()));
        }
    }
    Ok(())
}

/// `@devices.pci`: devmgr's PCI list (found through ACPI's MCFG and the PCIe
/// configuration space) is exactly QEMU's own (QMP `query-pci`) (TASK-041).
pub fn pci(ctx: &mut Ctx) -> Result<(), String> {
    use fjson::Value;
    ctx.expect_log("[devmgr] devmgr: ")?;
    let ours: std::collections::BTreeSet<(u64, u64, u64, u64, u64)> = ctx
        .door("devices")?
        .iter()
        .filter_map(|l| l.strip_prefix("pci "))
        .filter_map(|l| {
            let mut parts = l.split(' ');
            let (bdf, ids) = (parts.next()?, parts.next()?);
            let (bus, rest) = bdf.split_once(':')?;
            let (dev, func) = rest.split_once('.')?;
            let (vendor, id) = ids.split_once(':')?;
            let hex = |s: &str| u64::from_str_radix(s, 16).ok();
            Some((hex(bus)?, hex(dev)?, func.parse().ok()?, hex(vendor)?, hex(id)?))
        })
        .collect();
    let q = ctx.cdp.call("Freally.qemuPci", Value::Obj(Vec::new()))?;
    let mut theirs = std::collections::BTreeSet::new();
    for bus in q.as_array().ok_or("query-pci gave no list")? {
        for d in bus.get("devices").and_then(Value::as_array).map(<[Value]>::to_vec).unwrap_or_default() {
            let n = |k: &str| d.get(k).and_then(Value::as_i64).unwrap_or(-1) as u64;
            let id = |k: &str| d.get("id").and_then(|i| i.get(k)).and_then(Value::as_i64).unwrap_or(-1) as u64;
            theirs.insert((n("bus"), n("slot"), n("function"), id("vendor"), id("device")));
        }
    }
    if theirs.is_empty() {
        return Err("QEMU reported no PCI devices".into());
    }
    if ours != theirs {
        return Err(format!("devmgr found {ours:x?}; QEMU has {theirs:x?}"));
    }
    Ok(())
}

/// `@display.fill`: drv-gop-fb maps the framebuffer write-combining (a VMO over
/// exactly the range devmgr granted); a client fills it with the UI kit's
/// ground (`--color-bg`, #090c09), and the screen is that colour, every pixel
/// (TASK-042).
pub fn fill(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("[drv-gop-fb] drv-gop-fb: ")?;
    let answer = ctx.door("fbfill 090c09")?;
    if answer.first().map(String::as_str) != Some("ok") {
        return Err(format!("fbfill: {answer:?}"));
    }
    ctx.expect_screen(Expect::Fill(0x09_0c_09))?;
    // The fill went behind stage's back: stage draws the whole screen again
    // (light, then dark), so the next spec finds the desktop, not the fill.
    for theme in ["light", "dark"] {
        let said = ctx.door(&format!("stage theme {theme}"))?;
        if said.first().map(String::as_str) != Some("ok") {
            return Err(format!("stage theme {theme}: {said:?}"));
        }
    }
    ctx.expect_screen(Expect::Desktop)
}

/// The DOM key code and shift that type `c` on a US keyboard.
fn us_key(c: char) -> Option<(String, bool)> {
    let shifted_digits = ")!@#$%^&*(";
    let pairs = [
        (' ', ' ', "Space"),
        ('-', '_', "Minus"),
        ('=', '+', "Equal"),
        ('[', '{', "BracketLeft"),
        (']', '}', "BracketRight"),
        ('\\', '|', "Backslash"),
        (';', ':', "Semicolon"),
        ('\'', '"', "Quote"),
        ('`', '~', "Backquote"),
        (',', '<', "Comma"),
        ('.', '>', "Period"),
        ('/', '?', "Slash"),
    ];
    if c.is_ascii_lowercase() {
        return Some((format!("Key{}", c.to_ascii_uppercase()), false));
    }
    if c.is_ascii_uppercase() {
        return Some((format!("Key{c}"), true));
    }
    if c.is_ascii_digit() {
        return Some((format!("Digit{c}"), false));
    }
    if let Some(i) = shifted_digits.find(c) {
        return Some((format!("Digit{i}"), true));
    }
    // The unshifted character first: Space types ' ' either way.
    pairs.iter().find_map(|(plain, shifted, code)| {
        if c == *plain {
            Some((code.to_string(), false))
        } else if c == *shifted {
            Some((code.to_string(), true))
        } else {
            None
        }
    })
}

fn key(ctx: &mut Ctx, code: &str, down: bool) -> Result<(), String> {
    use fjson::Value;
    let kind = if down { "keyDown" } else { "keyUp" };
    ctx.cdp.call("Input.dispatchKeyEvent", Value::obj([("type", kind.into()), ("code", code.into())])).map(|_| ())
}

/// `@input.keys`: every printable ASCII character and every modifier, typed
/// over CDP, reaches drv-i8042 through IRQ 1 as scan code set 2 and comes out
/// as the right Freally key code (the HID usage) and the US layout's
/// character (TASK-043).
pub fn keys(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("[drv-i8042] drv-i8042: keyboard on set 2")?;
    for c in ' '..='~' {
        let (code, shift) = us_key(c).ok_or(format!("no US key types {c:?}"))?;
        if shift {
            key(ctx, "ShiftLeft", true)?;
        }
        key(ctx, &code, true)?;
        key(ctx, &code, false)?;
        if shift {
            key(ctx, "ShiftLeft", false)?;
        }
    }
    let opened = ctx.log_count("desktop: the launcher is open");
    let closed = ctx.log_count("desktop: the launcher is closed");
    for (code, usage) in [("ControlLeft", 0xE0), ("AltLeft", 0xE2), ("MetaLeft", 0xE3), ("ShiftRight", 0xE5)] {
        key(ctx, code, true)?;
        key(ctx, code, false)?;
        ctx.expect_log(&format!("drv-i8042: key {usage:#04x} up"))?;
    }
    // Meta alone is Super: it opened the launcher. Closed again, so the
    // desktop is left as found (input.mouse's click would start an app).
    ctx.expect_log_count("desktop: the launcher is open", opened + 1)?;
    key(ctx, "Escape", true)?;
    key(ctx, "Escape", false)?;
    ctx.expect_log_count("desktop: the launcher is closed", closed + 1)?;
    for c in ' '..='~' {
        let want = format!(" down U+{:04X}", c as u32);
        if ctx.log_count(&want) == 0 {
            return Err(format!("drv-i8042 never reported {c:?} (looked for {want:?})"));
        }
    }
    Ok(())
}

/// The sums of every mouse report drv-i8042 logged, as (dx, dy, wheel), and
/// the buttons each report held.
fn mouse_reports(ctx: &Ctx) -> ((i64, i64, i64), Vec<u8>) {
    let mut sum = (0, 0, 0);
    let mut buttons = Vec::new();
    for line in ctx.log_lines_with("drv-i8042: mouse dx ") {
        let words: Vec<&str> = line.split_whitespace().collect();
        let field = |name: &str| {
            words.iter().position(|w| *w == name).and_then(|i| words.get(i + 1)).and_then(|v| v.parse::<i64>().ok())
        };
        sum.0 += field("dx").unwrap_or(0);
        sum.1 += field("dy").unwrap_or(0);
        sum.2 += field("wheel").unwrap_or(0);
        buttons.push(field("buttons").unwrap_or(0) as u8);
    }
    (sum, buttons)
}

fn mouse_event(ctx: &mut Ctx, params: Vec<(&str, fjson::Value)>) -> Result<(), String> {
    let params = fjson::Value::Obj(params.into_iter().map(|(k, v)| (k.to_string(), v)).collect());
    ctx.cdp.call("Input.dispatchMouseEvent", params).map(|_| ())
}

/// Waits up to 10 s for the mouse's reports to add up to `want`.
fn expect_mouse_sum(ctx: &Ctx, want: (i64, i64, i64), what: &str) -> Result<(), String> {
    let start = std::time::Instant::now();
    loop {
        let (sum, _) = mouse_reports(ctx);
        if sum == want {
            return Ok(());
        }
        if start.elapsed() > std::time::Duration::from_secs(10) {
            return Err(format!("after {what}, the mouse's reports add up to {sum:?} (dx, dy, wheel), not {want:?}"));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// `@input.mouse`: the PS/2 mouse, found with its IntelliMouse wheel, reports
/// through IRQ 12 every movement on both axes, each button, and the wheel
/// both ways, in HID's directions (right, down and scrolling up positive)
/// (TASK-043).
pub fn mouse(ctx: &mut Ctx) -> Result<(), String> {
    use fjson::Value;
    ctx.expect_log("[drv-i8042] drv-i8042: keyboard on set 2; mouse with a wheel")?;
    let moved = |dx: f64, dy: f64| {
        vec![("type", "mouseMoved".into()), ("deltaX", Value::from(dx)), ("deltaY", Value::from(dy))]
    };
    mouse_event(ctx, moved(40.0, 0.0))?;
    expect_mouse_sum(ctx, (40, 0, 0), "40 px right")?;
    mouse_event(ctx, moved(-15.0, 25.0))?;
    expect_mouse_sum(ctx, (25, 25, 0), "15 px left and 25 down")?;
    for (button, bit) in [("left", 1u8), ("right", 2), ("middle", 4)] {
        let before = mouse_reports(ctx).1.len();
        mouse_event(ctx, vec![("type", "mousePressed".into()), ("button", button.into())])?;
        mouse_event(ctx, vec![("type", "mouseReleased".into()), ("button", button.into())])?;
        let start = std::time::Instant::now();
        loop {
            let held = mouse_reports(ctx).1;
            let since = &held[before.min(held.len())..];
            if since.contains(&bit) && since.last() == Some(&0) {
                break;
            }
            if start.elapsed() > std::time::Duration::from_secs(10) {
                return Err(format!("the {button} button's press and release came out as {since:?}, not [{bit}, 0]"));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    let wheel =
        |dy: f64| vec![("type", "mouseWheel".into()), ("deltaX", Value::from(0.0)), ("deltaY", Value::from(dy))];
    mouse_event(ctx, wheel(-120.0))?;
    expect_mouse_sum(ctx, (25, 25, 1), "one notch up")?;
    for _ in 0..3 {
        mouse_event(ctx, wheel(120.0))?;
    }
    expect_mouse_sum(ctx, (25, 25, -2), "three notches down")
}

/// `@input.absent`: on a machine with no PS/2 controller (QEMU's
/// `i8042=off`, whose FADT says so), drv-i8042 says "No PS/2 controller
/// found." and ends cleanly: never restarted, nothing else disturbed
/// (TASK-043; the PRD's P0 row).
pub fn absent(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("8042 absent")?;
    ctx.expect_log("[drv-i8042] drv-i8042: No PS/2 controller found.")?;
    ctx.expect_log("conductor: drv-i8042 ended (0)")?;
    // Well past the backoff's first steps (at once, 100 ms, 400 ms).
    std::thread::sleep(std::time::Duration::from_secs(2));
    ctx.expect_no_log("drv-i8042 ended (0); restarting")?;
    let told = ctx.log_count("No PS/2 controller found.");
    if told != 1 {
        return Err(format!("drv-i8042 said it found no controller {told} times, not once"));
    }
    // Stage doesn't wait on a driver that has ended: it says so, and its
    // first frame (S3) follows S2 within 500 ms.
    ctx.expect_log("stage: no keyboard or mouse")?;
    ctx.expect_log(" S3=")?;
    let line = ctx.log_line_with(" S3=").unwrap_or_default();
    let at = |name: &str| {
        line.split_whitespace()
            .find_map(|w| w.strip_prefix(name).and_then(|v| v.strip_prefix('=')))
            .and_then(|v| v.parse::<f64>().ok())
    };
    match (at("S2"), at("S3")) {
        (Some(s2), Some(s3)) if s3 >= s2 && s3 - s2 <= 500.0 => {}
        _ => return Err(format!("stage waited on the absent driver: {line}")),
    }
    let states = ctx.door("services")?;
    for want in ["svc drv-i8042 Stopped", "svc drv-gop-fb Running", "svc devmgr Running", "svc logger Running"] {
        if !states.iter().any(|s| s == want) {
            return Err(format!("the services aren't as they should be: wanted {want:?} in {states:?}"));
        }
    }
    Ok(())
}

/// `@serial.echo`: bytes through COM3 and back, every one: drv-uart16550's
/// interrupt-driven receive and send, and its flow control. The door stops
/// reading for 4 s first, so the driver's channel to it fills (`Busy`) and the
/// driver must hold the line (stop reading the UART, and so hold the sender
/// back) until the door catches up. 256 KB each run; the roadmap's 10 MB with
/// `cargo xtask cdp --slow` (closeouts): about 2 port I/O exits a byte, which
/// under KVM nested in WSL2 is ~16 KB/s, so ~11 min (TASK-044).
pub fn echo(ctx: &mut Ctx) -> Result<(), String> {
    let slow = std::env::var("FREALLY_CDP_SLOW").is_ok_and(|v| v == "1");
    let n: usize = if slow { 10 * 1024 * 1024 } else { 256 * 1024 };
    ctx.expect_log("[drv-uart16550] drv-uart16550: COM2 and COM3")?;
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let data: Vec<u8> = (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect();
    let start = std::time::Instant::now();
    let back = ctx.door_echo(&data, 4000)?;
    let secs = start.elapsed().as_secs_f64();
    if back != data {
        let at = back.iter().zip(&data).position(|(a, b)| a != b).unwrap_or(0);
        return Err(format!("the echo differs from the original at byte {at} of {n}"));
    }
    println!("    {} KB echoed without loss in {secs:.1} s, a 4 s stall included", n / 1024);
    Ok(())
}

/// `@serial.early`: a command sent the moment the VM restarts, before
/// drv-uart16550 or the door exists, is still answered: what arrives before a
/// client opens the port waits in the UART (the driver doesn't read a port no
/// one has opened, and keeps a byte already there when it turns the FIFO on)
/// (TASK-044).
pub fn early(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("conductor: test door open on COM3")?;
    ctx.reset()?;
    let answer = ctx.door("services").map_err(|e| format!("a command sent during boot was lost: {e}"))?;
    if !answer.iter().any(|l| l.starts_with("svc logger ")) {
        return Err(format!("a command sent during boot came back wrong: {answer:?}"));
    }
    Ok(())
}

/// Each pixel of a screenshot as (r, g, b).
fn rgb(shot: &fimage::png::Decoded) -> impl Iterator<Item = (usize, (u8, u8, u8))> + '_ {
    let n = shot.channels as usize;
    shot.pixels.chunks_exact(n).enumerate().map(|(i, p)| (i, (p[0], p[1], p[2])))
}

/// `@log.screen`: a dev image booted with `log=screen` (`\FREALLY\CMDLINE.TXT`)
/// and nothing to take the screen (no stage) shows the log on its own
/// screen: the heading in Freally green, the lines under it (TASK-074: a
/// machine with no serial port that stops before the desktop says why).
pub fn log_screen(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("freally-boot: boot options: log=screen")?;
    ctx.expect_log("logger: the log is on the screen until stage begins (log=screen)")?;
    ctx.expect_log("[devmgr] devmgr: ")?;
    std::thread::sleep(std::time::Duration::from_millis(800));
    let shot = ctx.screenshot()?;
    let w = shot.w as usize;
    let (mut green_top, mut text_rows) = (0, std::collections::BTreeSet::new());
    let bg = rgb(&shot).next().map(|(_, p)| p).unwrap_or_default();
    for (i, (r, g, b)) in rgb(&shot) {
        let y = i / w;
        if y < 40 && crate::greenish(r, g, b) {
            green_top += 1;
        }
        if y >= 40 && (r, g, b) != bg {
            text_rows.insert(y / 20);
        }
    }
    if green_top < 50 {
        return Err(format!("no green heading at the top ({green_top} green pixels)"));
    }
    if text_rows.len() < 10 {
        return Err(format!("hardly any log lines on the screen ({} rows with ink)", text_rows.len()));
    }
    ctx.keep_shot("log-screen")
}

/// `@log.screen_hands_over`: with stage in the image, the logger draws the
/// log only until stage begins, says so, and the desktop is what shows.
pub fn log_screen_hands_over(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("logger: the log is on the screen until stage begins (log=screen)")?;
    ctx.expect_log("logger: stage has the screen now; Monitor's Log tab has every line")?;
    ctx.expect_screen(Expect::Desktop)
}
