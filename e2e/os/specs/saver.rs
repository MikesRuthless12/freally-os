// SPDX-License-Identifier: Apache-2.0
//! The screensavers (TASK-077C, FR-091) on the real screen: one starts when
//! the machine is idle, any input ends it and does nothing else, Reduce
//! Motion shows the F still and dimmed, and each saver moves, shows the F's
//! green and keeps 30 frames a second (run at 2560×1600: the panel-16 EDID).

use crate::Ctx;
use crate::stage::key;
use std::time::{Duration, Instant};

fn has_saver(ctx: &Ctx) -> Result<bool, String> {
    Ok(ctx.door("stage list")?.iter().any(|l| l.ends_with("title=Screensaver")))
}

/// Waits up to `secs` for the saver to be up (or gone).
fn expect_saver(ctx: &Ctx, up: bool, secs: u64) -> Result<(), String> {
    let start = Instant::now();
    while has_saver(ctx)? != up {
        if start.elapsed() > Duration::from_secs(secs) {
            return Err(format!("the screensaver {} in {secs} s", if up { "never started" } else { "never ended" }));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// Clearly Freally-green pixels in a screenshot.
fn greens(shot: &fimage::png::Decoded) -> usize {
    shot.pixels
        .chunks(3)
        .filter(|p| {
            let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
            g > 90 && g > r + 30 && g > b + 30
        })
        .count()
}

/// Puts things back: no saver until a spec asks, full motion.
fn reset(ctx: &mut Ctx) -> Result<(), String> {
    if has_saver(ctx)? {
        key(ctx, "ShiftLeft", true)?;
        key(ctx, "ShiftLeft", false)?;
        expect_saver(ctx, false, 5)?;
    }
    ctx.door("stage saver 0 0")?;
    ctx.door("stage motion full")?;
    Ok(())
}

/// `@saver.starts_when_idle`: idle for the time set, the saver starts over
/// everything (the Rack along the foot is gone from the screen).
pub fn starts_when_idle(ctx: &mut Ctx) -> Result<(), String> {
    reset(ctx)?;
    let before = ctx.screenshot()?;
    ctx.door("stage saver 0 1500")?;
    expect_saver(ctx, true, 6)?;
    ctx.expect_log("stage: the screensaver starts (0)")?;
    ctx.expect_log("desktop: the screensaver: Bounce")?;
    std::thread::sleep(Duration::from_millis(300));
    let shot = ctx.screenshot()?;
    // The Rack's row: nothing of the desktop behind shows.
    let row = |s: &fimage::png::Decoded| {
        let y = s.h as usize - 20;
        s.pixels[y * s.w as usize * 3..(y + 1) * s.w as usize * 3].to_vec()
    };
    let same = row(&before).chunks(3).zip(row(&shot).chunks(3)).filter(|(a, b)| a == b).count();
    if same * 2 > shot.w as usize {
        return Err(format!("{same} of {} pixels in the Rack's row are as they were", shot.w));
    }
    ctx.keep_shot("saver-bounce")?;
    reset(ctx)
}

/// `@saver.ends_on_input`: a key ends it and does nothing else: Super, which
/// opens the launcher, doesn't.
pub fn ends_on_input(ctx: &mut Ctx) -> Result<(), String> {
    reset(ctx)?;
    ctx.door("stage saver 0 1000")?;
    expect_saver(ctx, true, 6)?;
    let opened = ctx.log_count("desktop: the launcher is open");
    key(ctx, "MetaLeft", true)?;
    key(ctx, "MetaLeft", false)?;
    expect_saver(ctx, false, 5)?;
    ctx.expect_log("stage: the screensaver ends")?;
    ctx.expect_log("desktop: the screensaver is gone")?;
    ctx.door("stage saver 0 0")?;
    std::thread::sleep(Duration::from_millis(500));
    if ctx.log_count("desktop: the launcher is open") != opened {
        return Err(String::from("the key that ended the screensaver also opened the launcher"));
    }
    // The next Super does what it always does.
    key(ctx, "MetaLeft", true)?;
    key(ctx, "MetaLeft", false)?;
    let start = Instant::now();
    while ctx.log_count("desktop: the launcher is open") == opened {
        if start.elapsed() > Duration::from_secs(5) {
            return Err(String::from("after the saver, Super didn't open the launcher"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    key(ctx, "Escape", true)?;
    key(ctx, "Escape", false)?;
    reset(ctx)
}

/// `@saver.reduce_motion`: the F still and dimmed: two frames a second
/// apart are the same, and nothing is as bright as the F at full light.
pub fn reduce_motion(ctx: &mut Ctx) -> Result<(), String> {
    reset(ctx)?;
    ctx.door("stage motion reduce")?;
    ctx.door("stage saver 0 1000")?;
    expect_saver(ctx, true, 6)?;
    ctx.expect_log("desktop: the screensaver: the still F")?;
    std::thread::sleep(Duration::from_millis(300));
    let a = ctx.screenshot()?;
    std::thread::sleep(Duration::from_secs(1));
    let b = ctx.screenshot()?;
    if a.pixels != b.pixels {
        return Err(String::from("the still F moved"));
    }
    let brightest = a.pixels.chunks(3).map(|p| p[1]).max().unwrap_or(0);
    if greens(&a) < 500 || brightest > 120 {
        return Err(format!("{} green pixels, the brightest green {brightest}: not the F, dimmed", greens(&a)));
    }
    ctx.keep_shot("saver-still")?;
    reset(ctx)
}

/// One saver: it moves, shows the F's green, and the desktop counts ≥ 30
/// frames a second at the screen's size.
fn runs(ctx: &mut Ctx, kind: u8, name: &str, shot: &str) -> Result<(), String> {
    reset(ctx)?;
    let said = ctx.log_count(&format!("desktop: the screensaver: {name}, "));
    ctx.door(&format!("stage saver {kind} 1000"))?;
    expect_saver(ctx, true, 6)?;
    std::thread::sleep(Duration::from_millis(800));
    let a = ctx.screenshot()?;
    std::thread::sleep(Duration::from_millis(700));
    let b = ctx.screenshot()?;
    if a.pixels == b.pixels {
        return Err(format!("{name} doesn't move"));
    }
    if greens(&b) < 2000 {
        return Err(format!("{name}: only {} green pixels", greens(&b)));
    }
    ctx.keep_shot(shot)?;
    // Its frame rate, as the desktop counts it every 5 s.
    let start = Instant::now();
    while ctx.log_count(&format!("desktop: the screensaver: {name}, ")) == said {
        if start.elapsed() > Duration::from_secs(12) {
            return Err(format!("{name}: the desktop never said its frame rate"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let line = ctx.log_line_with(&format!("desktop: the screensaver: {name}, ")).unwrap_or_default();
    let fps: u32 = line.split(", ").nth(1).and_then(|t| t.split(' ').next()).and_then(|n| n.parse().ok()).unwrap_or(0);
    println!("    {name}: {}", line.split("screensaver: ").nth(1).unwrap_or(&line));
    if fps < 30 {
        return Err(format!("{name}: {fps} frames a second (FR-091: at least 30): {line}"));
    }
    reset(ctx)
}

/// `@saver.bounce`
pub fn bounce(ctx: &mut Ctx) -> Result<(), String> {
    runs(ctx, 0, "Bounce", "saver-bounce-16")
}

/// `@saver.block`
pub fn block(ctx: &mut Ctx) -> Result<(), String> {
    runs(ctx, 1, "The turning block", "saver-block")
}

/// `@saver.swarm`
pub fn swarm(ctx: &mut Ctx) -> Result<(), String> {
    runs(ctx, 2, "Swarm", "saver-swarm")
}

/// `@saver.warp`
pub fn warp(ctx: &mut Ctx) -> Result<(), String> {
    runs(ctx, 3, "Warp", "saver-warp")
}
