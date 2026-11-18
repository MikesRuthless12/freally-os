// SPDX-License-Identifier: Apache-2.0
//! cdpd's methods through cdp-host (TASK-063): each answer checked against
//! what conductor, stage and the logger say through the test door.

use crate::Ctx;
use crate::sweep::{attr, click, q, q_all, type_keys, until_attr, until_q};
use fjson::Value;

fn call(ctx: &mut Ctx, method: &str, params: Value) -> Result<Value, String> {
    ctx.cdp.call(method, params).map_err(|e| format!("{method}: {e}"))
}

fn none() -> Value {
    Value::Obj(Vec::new())
}

fn arr<'a>(v: &'a Value, key: &str) -> Result<&'a [Value], String> {
    v.get(key).and_then(Value::as_array).ok_or(format!("no {key} in {}", v.to_json()))
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn n(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(Value::as_f64).unwrap_or(-1.0)
}

/// `@cdp.freally`: Target.getTargets, Freally.getProcesses,
/// getBootTimeline, getFaults, readBack, Performance.getMetrics and the
/// Log domain answer from the real services, and agree with them.
pub fn freally(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("desktop: the Rack is up")?;
    ctx.expect_log("cdpd: ready")?;
    // A desktop an earlier spec restarted has its window back within moments.
    let start = std::time::Instant::now();
    while !ctx.door("stage list")?.iter().any(|l| l.contains("owner=desktop")) {
        if start.elapsed() > std::time::Duration::from_secs(5) {
            return Err("the desktop has no window".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // Targets: the desktop first, then each window (the Rack's among them).
    let t = call(ctx, "Target.getTargets", none())?;
    let infos = arr(&t, "targetInfos")?;
    let windows = ctx.door("stage list")?.iter().filter(|l| l.starts_with("window ")).count();
    if s(&infos[0], "targetId") != "desktop" || infos.len() != windows + 1 {
        return Err(format!("{} targets for {windows} windows: {}", infos.len(), t.to_json()));
    }
    if !infos.iter().any(|i| s(i, "url") == "freally://app/desktop") {
        return Err(format!("no desktop window among the targets: {}", t.to_json()));
    }
    // Processes: conductor first, then the same services in the same states
    // as conductor's own list.
    let p = call(ctx, "Freally.getProcesses", none())?;
    let procs = arr(&p, "processes")?;
    if s(&procs[0], "name") != "conductor" || n(&procs[0], "pid") != 1.0 {
        return Err(format!("conductor (PID 1) isn't first: {}", procs[0].to_json()));
    }
    for line in ctx.door("services")? {
        let Some((name, state)) = line.strip_prefix("svc ").and_then(|l| l.split_once(' ')) else { continue };
        let got = procs.iter().find(|v| s(v, "name") == name).ok_or(format!("{name} isn't among the processes"))?;
        if s(got, "state") != state {
            return Err(format!("{name}: cdpd says {}, conductor {state}", s(got, "state")));
        }
    }
    let stage = procs.iter().find(|v| s(v, "name") == "stage").ok_or("no stage")?;
    if n(stage, "memBytes") < (1 << 20) as f64 || n(stage, "cpuNs") <= 0.0 {
        return Err(format!("stage's figures are empty: {}", stage.to_json()));
    }
    // The boot: B0 first, in order, through S4.
    let b = call(ctx, "Freally.getBootTimeline", none())?;
    let stages = arr(&b, "stages")?;
    let names: Vec<&str> = stages.iter().map(|v| s(v, "name")).collect();
    let ms: Vec<f64> = stages.iter().map(|v| n(v, "ms")).collect();
    if names.first() != Some(&"B0") || names.last() != Some(&"S4") || ms.windows(2).any(|w| w[1] < w[0]) {
        return Err(format!("the timeline: {}", b.to_json()));
    }
    let line = ctx.log_line_with(" S4=").unwrap_or_default();
    let s4 = *ms.last().unwrap_or(&0.0);
    if !line.contains(&format!("S4={:.1}", (s4 * 10.0).floor() / 10.0)) {
        return Err(format!("S4 is {s4} ms here, but conductor said: {line}"));
    }
    // Metrics agree with the rest.
    let m = call(ctx, "Performance.getMetrics", none())?;
    let metric =
        |k: &str| arr(&m, "metrics").ok().and_then(|a| a.iter().find(|v| s(v, "name") == k)).map(|v| n(v, "value"));
    let running = procs.iter().filter(|v| s(v, "state") == "Running").count() as f64;
    let restarts: f64 = procs.iter().map(|v| n(v, "restarts")).sum();
    if metric("BootMs") != Some(s4)
        || metric("ProcessCount") != Some(running)
        || metric("ServiceRestarts") != Some(restarts)
        || metric("MemUsed").is_none_or(|v| v < (16 << 20) as f64)
        || metric("FirmwareMs").is_none()
    {
        return Err(format!("the metrics: {}", m.to_json()));
    }
    // Faults: what conductor holds since the start; a later cursor sees none.
    let f = call(ctx, "Freally.getFaults", Value::obj([("sinceSeq", 0u32.into())]))?;
    let last = n(&f, "lastSeq");
    let later = call(ctx, "Freally.getFaults", Value::obj([("sinceSeq", Value::Num(last))]))?;
    if !arr(&later, "faults")?.is_empty() {
        return Err(format!("faults after the last one: {}", later.to_json()));
    }
    // readBack: the theme stage holds, before and after the door changes it.
    let theme = |ctx: &mut Ctx| {
        call(ctx, "Freally.readBack", Value::obj([("service", "stage".into()), ("query", "theme".into())]))
            .map(|v| s(&v, "value").to_string())
    };
    if theme(ctx)? != "Dark" {
        return Err("stage's theme isn't Dark at the start".into());
    }
    crate::stage::ok(ctx, "stage theme light")?;
    let light = theme(ctx)?;
    crate::stage::ok(ctx, "stage theme dark")?;
    if light != "Light" || theme(ctx)? != "Dark" {
        return Err(format!("readBack saw {light} after the theme went light"));
    }
    if call(ctx, "Freally.readBack", Value::obj([("service", "nobody".into()), ("query", "x".into())])).is_ok() {
        return Err("readBack answered for a service it can't read".into());
    }
    // Log: after Log.enable, records come as Log.entryAdded events, levels
    // in CDP's words. A fresh line (Shift down and up: e2e images log keys),
    // read until it arrives (500 records an answer; an earlier spec may have
    // restarted the logger, so boot's lines can't be counted on).
    ctx.cdp.take_events();
    call(ctx, "Log.enable", none())?;
    crate::stage::key(ctx, "ShiftLeft", true)?;
    crate::stage::key(ctx, "ShiftLeft", false)?;
    let mut seen = 0;
    let mut found = false;
    for _ in 0..40 {
        call(ctx, "Freally.getBootTimeline", none())?;
        let events = ctx.cdp.take_events();
        let entries: Vec<&Value> = events
            .iter()
            .filter(|e| s(e, "method") == "Log.entryAdded")
            .filter_map(|e| e.get("params")?.get("entry"))
            .collect();
        seen += entries.len();
        if entries.iter().any(|e| {
            s(e, "source") == "drv-i8042"
                && s(e, "text").starts_with("drv-i8042: key 0xe1 down")
                && s(e, "level") == "info"
        }) {
            found = true;
            break;
        }
    }
    call(ctx, "Log.disable", none())?;
    if !found {
        return Err(format!("no Log.entryAdded for the Shift press among {seen} entries"));
    }
    Ok(())
}

/// `@cdp.dom`: the desktop's apps as a document: the Rack's keys, Monitor's
/// tabs and sort headers, the launcher's search and rows, the terminal's
/// screen. Each is found by a selector, worked by its box on screen with
/// the real mouse and keyboard, and read back from its attributes (and the
/// app's own door) (TASK-063).
pub fn dom(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("desktop: the Rack is up")?;
    ctx.expect_log("cdpd: ready")?;
    const RACK: &str = "WINDOW[data-owner=\"desktop\"]";
    let apps = until_q(ctx, &format!("{RACK} BUTTON[data-testid=\"rack-launcher\"]"))?;
    if attr(ctx, apps, "aria-label")?.as_deref() != Some("Apps") {
        return Err("the launcher key isn't called Apps".into());
    }
    // Monitor from its Rack key: its tabs appear; the key says it's running.
    let key = until_q(ctx, &format!("{RACK} BUTTON[data-testid=\"rack-monitor\"]"))?;
    click(ctx, key)?;
    const MON: &str = "WINDOW[data-owner=\"monitor\"]";
    let boot = until_q(ctx, &format!("{MON} TABLIST TAB[aria-label=\"Boot\"]"))?;
    until_attr(ctx, &format!("{RACK} [data-testid=\"rack-monitor\"]"), "aria-valuenow", "running")?;
    // A tab by its box: selected, the others not, and Monitor agrees.
    click(ctx, boot)?;
    until_attr(ctx, &format!("{MON} [data-testid=\"tab-boot\"]"), "aria-selected", "true")?;
    let procs = q(ctx, &format!("{MON} [data-testid=\"tab-processes\"]"))?;
    if attr(ctx, procs, "aria-selected")?.as_deref() != Some("false") {
        return Err("Processes is still selected".into());
    }
    if !ctx.door("monitor state")?.iter().any(|l| l == "tab Boot") {
        return Err("Monitor doesn't say Boot".into());
    }
    if q(ctx, &format!("{MON} TABLE ROW[aria-label=\"S4\"]"))? == 0 {
        return Err("no S4 row under Boot".into());
    }
    // Back to Processes; sort by Memory: the header says so, Monitor too.
    let procs = q(ctx, &format!("{MON} [data-testid=\"tab-processes\"]"))?;
    click(ctx, procs)?;
    let memory = until_q(ctx, &format!("{MON} BUTTON[data-testid=\"sort-memory\"]"))?;
    click(ctx, memory)?;
    until_attr(ctx, &format!("{MON} [data-testid=\"sort-memory\"]"), "aria-pressed", "true")?;
    if !ctx.door("monitor state")?.iter().any(|l| l.starts_with("sort Memory")) {
        return Err("Monitor isn't sorted by memory".into());
    }
    if q_all(ctx, &format!("{MON} BUTTON[aria-pressed=\"true\"]"))?.len() != 1 {
        return Err("more than one header says it sorts".into());
    }
    // The launcher: Super opens it, its search focused; typing narrows the
    // rows; a click on one starts its app and closes the launcher.
    crate::stage::key(ctx, "MetaLeft", true)?;
    crate::stage::key(ctx, "MetaLeft", false)?;
    const SEARCH: &str = "[data-testid=\"launcher-search\"]";
    until_attr(ctx, SEARCH, "data-focused", "true")?;
    type_keys(ctx, "term")?;
    until_attr(ctx, SEARCH, "aria-valuenow", "term")?;
    let rows = q_all(ctx, "LISTBOX OPTION")?;
    if rows.len() != 1 || attr(ctx, rows[0], "aria-label")?.as_deref() != Some("Terminal") {
        return Err(format!("{} rows for \"term\"", rows.len()));
    }
    let started = ctx.log_count("desktop: started terminal");
    click(ctx, rows[0])?;
    ctx.expect_log_count("desktop: started terminal", started + 1)?;
    let start = std::time::Instant::now();
    while q(ctx, SEARCH)? != 0 {
        if start.elapsed() > std::time::Duration::from_secs(5) {
            return Err("the launcher stayed open".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // The terminal: brought forward by Target.activateTarget; what's typed
    // is its screen's value.
    let term = until_q(ctx, "WINDOW[data-owner=\"terminal\"]")?;
    let wid = attr(ctx, term, "data-window-id")?.ok_or("no window id")?;
    call(ctx, "Target.activateTarget", Value::obj([("targetId", wid.as_str().into())]))?;
    const SCREEN: &str = "WINDOW[data-owner=\"terminal\"] TEXTBOX[data-testid=\"terminal-screen\"]";
    until_attr(ctx, SCREEN, "data-focused", "true")?;
    type_keys(ctx, "ver")?;
    let start = std::time::Instant::now();
    loop {
        let screen = q(ctx, SCREEN)?;
        let v = attr(ctx, screen, "aria-valuenow")?.unwrap_or_default();
        if v.ends_with("$ ver") {
            break;
        }
        if start.elapsed() > std::time::Duration::from_secs(10) {
            return Err(format!("the screen's line is {v:?}"));
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    for _ in 0..3 {
        crate::stage::key(ctx, "Backspace", true)?;
        crate::stage::key(ctx, "Backspace", false)?;
    }
    // The AX tree says the same in its own words.
    let ax = call(ctx, "Accessibility.getFullAXTree", none())?;
    let has = |role: &str, name: &str| {
        arr(&ax, "nodes").is_ok_and(|n| {
            n.iter().any(|v| {
                v.get("role").and_then(|r| r.get("value")).and_then(Value::as_str) == Some(role)
                    && v.get("name").and_then(|r| r.get("value")).and_then(Value::as_str) == Some(name)
            })
        })
    };
    if !has("tab", "Boot") || !has("textbox", "Terminal") || !has("button", "Apps") {
        return Err("the AX tree misses Monitor's Boot tab, the terminal or the Apps key".into());
    }
    Ok(())
}
