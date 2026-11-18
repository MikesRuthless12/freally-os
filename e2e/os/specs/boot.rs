// SPDX-License-Identifier: Apache-2.0
//! The boot specs (`@boot.*`): the bootloader's screens, compared with what
//! `fscreen` draws on the host for the same screen size, pixel for pixel.

use crate::{Ctx, Expect};

/// `@boot.title`: verified, and the title painted.
pub fn title(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_screen(Expect::Title { note: "Freally Core is running." })?;
    ctx.expect_log("freally-boot: verified Freally OS")?;
    ctx.expect_log("signed by the dev key")
}

/// `@boot.integrity`: a flipped bit in block 2 is refused, with the reason shown.
pub fn integrity(ctx: &mut Ctx) -> Result<(), String> {
    ctx.allow_errors(&["image refused"]);
    ctx.expect_screen(Expect::Integrity { reason: fsi::FsiError::BadBlock(2).describe() })?;
    ctx.expect_log("BadBlock(2)")?;
    ctx.expect_no_log("showing the title")
}

/// `@boot.release`: the release bootloader boots its staging-signed image.
pub fn release(ctx: &mut Ctx) -> Result<(), String> {
    // A release image's logs stay off COM1: the screen is the proof it came
    // up, stage's first frame (the ground; the desktop's wallpaper, TASK-059).
    ctx.expect_screen(Expect::Fill(fui::tokens_gen::dark::COLOR_BG.xrgb()))?;
    ctx.expect_log("freally-boot 0.1.0 (release)")?;
    ctx.expect_log("signed by the staging key")
}

/// `@boot.seal`: a release bootloader refuses a dev-signed image.
pub fn seal(ctx: &mut Ctx) -> Result<(), String> {
    ctx.allow_errors(&["image refused"]);
    ctx.expect_screen(Expect::Integrity { reason: fsi::FsiError::UnknownKey.describe() })?;
    ctx.expect_log("(UnknownKey)")?;
    ctx.expect_no_log("showing the title")
}

/// `@boot.handoff`: boot services exit, the runtime services move to the direct
/// map, and Freally Core is entered with a BootInfo it accepts (TASK-018).
/// `@boot.conductor`: Freally Core starts conductor from the image as PID 1
/// (its ELF re-checked against the TOC, loaded W^X); conductor says it started,
/// and the kernel stamps C0 when it first enters user mode (TASK-030).
pub fn conductor(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("freally-core: scheduler ready (SMEP")?;
    ctx.expect_log("freally-core: conductor started (C0 ")?;
    ctx.expect_log("[pid 1] conductor: started")?;
    let line = ctx.log_line_with("conductor started (C0 ").ok_or("no C0 line")?;
    let ms: u64 = line
        .split("(C0 ")
        .nth(1)
        .and_then(|s| s.split(' ').next())
        .and_then(|n| n.parse().ok())
        .ok_or("the C0 line has no time")?;
    if ms == 0 || ms > 3_000 {
        return Err(format!("C0 came {ms} ms after B0 (the boot budget is 3,000 ms)"));
    }
    desktop(ctx)
}

/// Stage's first frame is on the screen: the dark ground, everywhere (the
/// desktop's wallpaper and Rack join it in TASK-059). From S3 the screen is
/// stage's; the bootloader's title stays only in `@boot.title`'s image,
/// which has no stage.
fn desktop(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("stage: first frame")?;
    ctx.expect_screen(Expect::Fill(fui::tokens_gen::dark::COLOR_BG.xrgb()))
}

pub fn handoff(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("freally-boot: boot services exited")?;
    ctx.expect_log("freally-boot: runtime services moved")?;
    ctx.expect_log("freally-boot: jumping to Freally Core")?;
    ctx.expect_log("freally-core 0.1.0 entered")?;
    ctx.expect_log("freally-core: memory ready")?;
    desktop(ctx)
}

/// Resets the VM (QMP system_reset, as a restart button does) and waits for
/// the `n`th line containing `text`.
fn reset_until(ctx: &mut Ctx, text: &str, n: usize) -> Result<(), String> {
    ctx.reset()?;
    let start = std::time::Instant::now();
    while ctx.log_count(text) < n {
        if start.elapsed() > std::time::Duration::from_secs(60) {
            return Err(format!("after a restart, COM1 said {text:?} {} times, not {n}", ctx.log_count(text)));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    Ok(())
}

/// `@boot.fallback`: slot B on trial, its image crashing in Freally Core
/// before conductor, crashes three times; the fourth boot lands on slot A,
/// and the log says the update didn't start (TASK-045, FR-003).
pub fn fallback(ctx: &mut Ctx) -> Result<(), String> {
    ctx.allow_errors(&["freally-core: ERROR", "showing the panic screen", "can't start", "didn't start"]);
    ctx.expect_log("[conductor] conductor: started from slot A")?;
    let answer = ctx.door("bootctl trial b")?;
    if answer != ["ok"] {
        return Err(format!("bootctl trial b: {answer:?}"));
    }
    let fell_back = "freally-boot: slot B can't start (Bad); booting slot A";
    let gave_up = ctx.log_count(fell_back);
    for left in [2, 1, 0] {
        let crashes = ctx.log_count("freally-core: ERROR");
        reset_until(ctx, &format!("freally-boot: booting slot B (trial, {left} tries left)"), 1)?;
        reset_wait_crash(ctx, crashes + 1)?;
    }
    let said = ctx.log_count("didn't start, so Freally OS went back to");
    reset_until(ctx, fell_back, gave_up + 1)?;
    ctx.expect_log_count("didn't start, so Freally OS went back to", said + 1)?;
    let state = ctx.door("bootctl")?;
    for want in ["active A", "booted A"] {
        if !state.iter().any(|l| l == want) {
            return Err(format!("after the fallback the record says {state:?}, wanted {want:?}"));
        }
    }
    if !state.iter().any(|l| l.starts_with("slot B Bad 0")) {
        return Err(format!("slot B isn't Bad after three failed trial boots: {state:?}"));
    }
    Ok(())
}

/// Waits for the crash-test kernel's panic (the `n`th ERROR line).
fn reset_wait_crash(ctx: &mut Ctx, n: usize) -> Result<(), String> {
    ctx.expect_log_count("freally-core: ERROR", n).map_err(|e| format!("the trial image didn't crash: {e}"))
}

/// `@boot.trial`: slot B on trial with a good image boots with a try spent,
/// and conductor marks it Good through UEFI SetVariable once every boot
/// service has run for 10 s; the next boot stays on B, no try spent
/// (TASK-045, FR-003).
pub fn trial(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("[conductor] conductor: started from slot")?;
    let answer = ctx.door("bootctl trial b")?;
    if answer != ["ok"] {
        return Err(format!("bootctl trial b: {answer:?}"));
    }
    let booted = ctx.log_count("freally-boot: booting slot B (trial, 2 tries left)");
    reset_until(ctx, "freally-boot: booting slot B (trial, 2 tries left)", booted + 1)?;
    let marked = "started cleanly: marked Good";
    let start = std::time::Instant::now();
    while ctx.log_count(marked) == 0 {
        if start.elapsed() > std::time::Duration::from_secs(30) {
            return Err("conductor never marked the trial slot Good".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let state = ctx.door("bootctl")?;
    if !state.iter().any(|l| l.starts_with("slot B Good")) || !state.iter().any(|l| l == "booted B") {
        return Err(format!("after the mark the record says {state:?}"));
    }
    // A Good slot boots with no try spent, and isn't marked again.
    let plain = ctx.log_count("freally-boot: booting slot B");
    let trials = ctx.log_count("freally-boot: booting slot B (trial");
    reset_until(ctx, "freally-boot: booting slot B", plain + 1)?;
    ctx.expect_log("conductor: started from slot B (Good")?;
    if ctx.log_count("freally-boot: booting slot B (trial") != trials {
        return Err("a slot already Good was booted as a trial".into());
    }
    Ok(())
}

/// `@boot.timeline`: conductor's `boot:` line, said again when stage stamps
/// S3, holds every stage from B0 to S3, in order, each no earlier than the
/// one before, B0 at 0 (K3, all CPUs online, joins with SMP: TASK-036); and
/// the firmware's own times from the FPDT, F0 before F1, or that there's no
/// FPDT (TASK-047, TASK-057, FR-004).
pub fn timeline(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log(" S3=")?;
    let line = ctx
        .log_lines_with("conductor: boot: ")
        .into_iter()
        .find(|l| l.contains(" S3="))
        .ok_or("no boot line with S3")?;
    let body = line.split("conductor: boot: ").nth(1).unwrap_or_default();
    let (stages, firmware) = body.split_once("ms after B0; firmware ").ok_or(format!("unreadable: {line}"))?;
    let mut seen: Vec<(String, f64)> = Vec::new();
    for pair in stages.split_whitespace() {
        let (name, ms) = pair.split_once('=').ok_or(format!("unreadable stage {pair:?} in {line}"))?;
        let ms: f64 = ms.parse().map_err(|_| format!("unreadable time {pair:?} in {line}"))?;
        seen.push((name.to_string(), ms));
    }
    let want = ["B0", "B1", "B2", "B3", "K0", "K1", "K2", "C0", "S1", "S2", "S3"];
    let names: Vec<&str> = seen.iter().map(|(n, _)| n.as_str()).filter(|n| *n != "K3").collect();
    if names != want {
        return Err(format!("the stages are {names:?}, not {want:?}: {line}"));
    }
    if seen[0].1 != 0.0 {
        return Err(format!("B0 isn't 0: {line}"));
    }
    if let Some(w) = seen.windows(2).find(|w| w[1].1 < w[0].1) {
        return Err(format!("{} ({} ms) comes before {} ({} ms): {line}", w[1].0, w[1].1, w[0].0, w[0].1));
    }
    if firmware != "times: no FPDT" {
        let t: Vec<f64> = firmware
            .trim_end_matches(" ms on its own clock")
            .split_whitespace()
            .filter_map(|p| p.split_once('=').and_then(|(_, v)| v.parse().ok()))
            .collect();
        if t.len() != 2 || t[0] > t[1] {
            return Err(format!("the firmware's times don't add up: {line}"));
        }
    }
    println!("    {body}");
    Ok(())
}
