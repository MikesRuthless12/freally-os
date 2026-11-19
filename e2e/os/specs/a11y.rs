// SPDX-License-Identifier: Apache-2.0
//! Everything without a pointer (TASK-064, FR-027): the keyboard alone opens
//! apps, moves round their controls, works them and closes them.

use crate::Ctx;
use crate::stage::key;
use crate::sweep::{type_keys, until_attr, until_q};
use std::time::{Duration, Instant};

fn press(ctx: &mut Ctx, code: &str) -> Result<(), String> {
    key(ctx, code, true)?;
    key(ctx, code, false)
}

fn with(ctx: &mut Ctx, held: &str, code: &str) -> Result<(), String> {
    key(ctx, held, true)?;
    press(ctx, code)?;
    key(ctx, held, false)
}

/// Waits up to 10 s for Monitor's door to say `want`.
fn monitor_says(ctx: &mut Ctx, want: impl Fn(&str) -> bool) -> Result<(), String> {
    let start = Instant::now();
    loop {
        let lines = ctx.door("monitor state")?;
        if lines.iter().any(|l| want(l)) {
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err(format!("Monitor says {lines:?}"));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

const MON: &str = "WINDOW[data-owner=\"monitor\"]";

fn focused(ctx: &mut Ctx, testid: &str) -> Result<(), String> {
    until_attr(ctx, &format!("{MON} [data-testid=\"{testid}\"]"), "data-focused", "true").map_err(|e| {
        // Which control has it instead.
        let has = crate::sweep::q(ctx, &format!("{MON} [data-focused=\"true\"]"))
            .ok()
            .filter(|&id| id != 0)
            .and_then(|id| crate::sweep::attr(ctx, id, "data-testid").ok().flatten());
        // And how Monitor is: running, its CPU time, its memory.
        let mon = ctx
            .cdp
            .call("Freally.getProcesses", fjson::Value::Obj(Vec::new()))
            .ok()
            .and_then(|r| r.get("processes").and_then(fjson::Value::as_array).map(<[fjson::Value]>::to_vec))
            .and_then(|p| p.into_iter().find(|p| p.get("name").and_then(fjson::Value::as_str) == Some("monitor")))
            .map(|p| p.to_json())
            .unwrap_or_default();
        format!("{e} (the focus is on {has:?}; Monitor: {mon})")
    })
}

/// `@a11y.keyboard`: Super opens the launcher; typing and Enter start
/// Monitor. Tab and Shift+Tab walk Monitor's controls (the tabs, then the
/// tab's own), the ring and the tree say which has focus, and Enter and
/// Space work it: a sort by Memory, then by State, the Log tab, its chips.
/// Super+T and typing reach the shell; Alt+Tab moves between windows;
/// Alt+F4 closes them (TASK-064, FR-027).
pub fn keyboard(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("desktop: the Rack is up")?;
    // The launcher: Super, "mon", Enter.
    let started = ctx.log_count("desktop: started monitor");
    press(ctx, "MetaLeft")?;
    until_attr(ctx, "[data-testid=\"launcher-search\"]", "data-focused", "true")?;
    type_keys(ctx, "mon")?;
    until_attr(ctx, "LISTBOX OPTION[data-testid=\"launcher-monitor\"]", "aria-selected", "true")?;
    press(ctx, "Enter")?;
    ctx.expect_log_count("desktop: started monitor", started + 1)?;
    until_q(ctx, &format!("{MON} TAB"))?;
    until_attr(ctx, MON, "data-focused", "true")?;
    // Tab: the first control; seven more (the other four tabs, Service,
    // State): Sort by Memory; Enter sorts.
    press(ctx, "Tab")?;
    focused(ctx, "tab-processes")?;
    for _ in 0..7 {
        press(ctx, "Tab")?;
    }
    focused(ctx, "sort-memory")?;
    press(ctx, "Enter")?;
    monitor_says(ctx, |l| l.starts_with("sort Memory"))?;
    // Shift+Tab: back to State; Space sorts by it.
    with(ctx, "ShiftLeft", "Tab")?;
    focused(ctx, "sort-state")?;
    press(ctx, "Space")?;
    monitor_says(ctx, |l| l.starts_with("sort State"))?;
    // Three back (Service, Devices): the Log tab; Enter shows it; Tab to
    // the chips, which move the filter on.
    for _ in 0..3 {
        with(ctx, "ShiftLeft", "Tab")?;
    }
    focused(ctx, "tab-log")?;
    press(ctx, "Enter")?;
    monitor_says(ctx, |l| l == "tab Log")?;
    // Past the Devices tab to the chips.
    press(ctx, "Tab")?;
    focused(ctx, "tab-devices")?;
    press(ctx, "Tab")?;
    focused(ctx, "chip-service")?;
    press(ctx, "Enter")?;
    monitor_says(ctx, |l| l.starts_with("filter ") && !l.starts_with("filter all"))?;
    press(ctx, "Tab")?;
    focused(ctx, "chip-level")?;
    press(ctx, "Space")?;
    monitor_says(ctx, |l| l.starts_with("filter ") && l.ends_with(" 1"))?;
    // Tab past the last wraps to the first.
    press(ctx, "Tab")?;
    focused(ctx, "tab-processes")?;
    // Super+T: the terminal, and the shell takes what's typed.
    with(ctx, "MetaLeft", "KeyT")?;
    const TERM: &str = "WINDOW[data-owner=\"terminal\"]";
    until_attr(ctx, TERM, "data-focused", "true")?;
    type_keys(ctx, "version")?;
    press(ctx, "Enter")?;
    let start = Instant::now();
    loop {
        let rows = ctx.door("terminal screen")?;
        if rows.iter().any(|l| l.contains("Freally OS 0.")) {
            break;
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err(format!("`version` by keyboard printed {rows:?}"));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    // Alt+Tab: back to Monitor; again: the terminal.
    with(ctx, "AltLeft", "Tab")?;
    until_attr(ctx, MON, "data-focused", "true")?;
    with(ctx, "AltLeft", "Tab")?;
    until_attr(ctx, TERM, "data-focused", "true")?;
    // Alt+F4 closes the focused window (the terminal), then Monitor.
    with(ctx, "AltLeft", "F4")?;
    crate::sweep::until_gone(ctx, TERM)?;
    until_attr(ctx, MON, "data-focused", "true")?;
    with(ctx, "AltLeft", "F4")?;
    crate::sweep::until_gone(ctx, MON)?;
    Ok(())
}
