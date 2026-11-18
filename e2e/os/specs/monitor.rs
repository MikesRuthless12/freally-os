// SPDX-License-Identifier: Apache-2.0
//! Monitor (TASK-062): every tab, the sort and the filters, on the real
//! screen with the real mouse and keyboard, read back from Monitor itself.

use crate::Ctx;
use crate::stage::{button, key, move_to};
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
struct State {
    tab: String,
    sort: String,
    filter: String,
    lines: Vec<String>,
}

fn state(ctx: &Ctx) -> Result<State, String> {
    let mut s = State::default();
    for l in ctx.door("monitor state")? {
        if let Some(v) = l.strip_prefix("tab ") {
            s.tab = v.into();
        } else if let Some(v) = l.strip_prefix("sort ") {
            s.sort = v.into();
        } else if let Some(v) = l.strip_prefix("filter ") {
            s.filter = v.into();
        } else if let Some(v) = l.strip_prefix("line ") {
            s.lines.push(v.into());
        } else if l.starts_with("error") {
            return Err(l);
        }
    }
    Ok(s)
}

fn until(ctx: &Ctx, what: &str, want: impl Fn(&State) -> bool) -> Result<State, String> {
    let start = Instant::now();
    loop {
        if let Ok(s) = state(ctx)
            && want(&s)
        {
            return Ok(s);
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err(format!("{what}: {:?}", state(ctx)));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Monitor's window frame (x, y).
fn frame(ctx: &Ctx) -> Result<(i32, i32), String> {
    let lines = ctx.door("stage list")?;
    let l = lines.iter().find(|l| l.contains("owner=monitor")).ok_or(format!("no Monitor window: {lines:?}"))?;
    let num = |n: &str| {
        l.split_whitespace()
            .find_map(|w| w.strip_prefix(n).and_then(|v| v.strip_prefix('=')))
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(-1)
    };
    Ok((num("x"), num("y")))
}

/// A click at (x, y) of Monitor's surface.
fn click(ctx: &mut Ctx, (fx, fy): (i32, i32), x: i32, y: i32) -> Result<(), String> {
    move_to(ctx, fx + x, fy + 32 + y)?;
    button(ctx, true)?;
    button(ctx, false)
}

/// `@monitor.tabs`: Super+M opens Monitor; Processes lists every service
/// with its state, memory, CPU % and restarts, sorted by a header click
/// (again: the other way); Boot shows each stage and the summary; Faults
/// says nothing has crashed; Log is live and its chips filter it; Ctrl+Tab
/// steps the tabs (TASK-062, FR-034).
pub fn tabs(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("desktop: the Rack is up")?;
    key(ctx, "MetaLeft", true)?;
    key(ctx, "KeyM", true)?;
    key(ctx, "KeyM", false)?;
    key(ctx, "MetaLeft", false)?;
    ctx.expect_log("monitor: open")?;
    let s = until(ctx, "Processes, sampled twice", |s| {
        s.tab == "Processes" && s.lines.iter().any(|l| l.starts_with("stage |") && l.contains(" %"))
    })?;
    // Every process that's up, conductor (PID 1) too, and each Running: the
    // desktop has no endpoint, its S4 stamp says it's up.
    for svc in ["conductor", "logger", "stage", "desktop", "monitor"] {
        if !s.lines.iter().any(|l| l.starts_with(&format!("{svc} | Running |"))) {
            return Err(format!("{svc} isn't listed as Running: {:?}", s.lines));
        }
    }
    if !s.lines.iter().any(|l| l.contains("MiB") || l.contains("KiB")) {
        return Err(format!("no memory figures: {:?}", s.lines));
    }
    let f = frame(ctx)?;
    // Sort by Service, then the other way.
    click(ctx, f, 20, 60)?;
    let s = until(ctx, "sorted by name", |s| s.sort == "Name up")?;
    let names: Vec<&str> = s.lines.iter().map(|l| l.split(" |").next().unwrap_or("")).collect();
    let mut sorted = names.clone();
    sorted.sort();
    if names != sorted {
        return Err(format!("not sorted by name: {names:?}"));
    }
    click(ctx, f, 20, 60)?;
    until(ctx, "sorted the other way", |s| s.sort == "Name down")?;
    // Boot.
    click(ctx, f, 16 + 140 + 30, 20)?;
    until(ctx, "the Boot tab", |s| {
        s.tab == "Boot"
            && s.lines.iter().any(|l| l.starts_with("S4 "))
            && s.lines.iter().any(|l| l.contains("Freally OS"))
    })?;
    // Faults.
    click(ctx, f, 16 + 280 + 30, 20)?;
    until(ctx, "the Faults tab", |s| s.tab == "Faults")?;
    // Log, and its service chip.
    click(ctx, f, 16 + 420 + 30, 20)?;
    let s = until(ctx, "the Log tab", |s| s.tab == "Log" && !s.lines.is_empty())?;
    let all = s.lines.len();
    click(ctx, f, 30, 66)?;
    let s = until(ctx, "a service chosen", |s| !s.filter.starts_with("all"))?;
    let svc = s.filter.split_whitespace().next().unwrap_or("").to_string();
    if s.lines.iter().any(|l| !l.starts_with(&format!("{svc} "))) || s.lines.len() > all {
        return Err(format!("the filter let others through: {:?}", s.lines));
    }
    // Ctrl+Tab: back round to Processes.
    key(ctx, "ControlLeft", true)?;
    key(ctx, "Tab", true)?;
    key(ctx, "Tab", false)?;
    key(ctx, "ControlLeft", false)?;
    until(ctx, "Ctrl+Tab to Processes", |s| s.tab == "Processes")?;
    ctx.keep_shot("monitor")?;
    Ok(())
}
