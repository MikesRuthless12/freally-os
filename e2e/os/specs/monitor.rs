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
            // The firmware's maker (BootInfo v3: Ubuntu's OVMF says "Ubuntu
            // distribution of EDK II", with no
            // FPDT) and the clock's source (TASK-073).
            && s.lines.iter().any(|l| l.contains("EDK II firmware doesn't report its own time"))
            && s.lines.iter().any(|l| l.starts_with("TSC ") && l.contains(" MHz, "))
    })?;
    // Faults.
    click(ctx, f, 16 + 280 + 30, 20)?;
    until(ctx, "the Faults tab", |s| s.tab == "Faults")?;
    // Log, and its service chip.
    click(ctx, f, 16 + 420 + 30, 20)?;
    let s = until(ctx, "the Log tab", |s| s.tab == "Log" && !s.lines.is_empty())?;
    let before = s.filter.clone();
    click(ctx, f, 30, 66)?;
    let s = until(ctx, "the service chip moved on", |s| s.filter != before)?;
    let svc = s.filter.split_whitespace().next().unwrap_or("").to_string();
    if svc != "all" && s.lines.iter().any(|l| !l.starts_with(&format!("{svc} "))) {
        return Err(format!("the filter let others through: {:?}", s.lines));
    }
    // Ctrl+Tab: on to Devices, then back round to Processes.
    for want in ["Devices", "Processes"] {
        key(ctx, "ControlLeft", true)?;
        key(ctx, "Tab", true)?;
        key(ctx, "Tab", false)?;
        key(ctx, "ControlLeft", false)?;
        until(ctx, &format!("Ctrl+Tab to {want}"), |s| s.tab == want)?;
    }
    // The CPU graph at the foot of Processes fills a sample a second, in the
    // plot kit's pen (TASK-075A).
    let sel = "WINDOW[data-owner=\"monitor\"] [data-testid=\"cpu-graph\"]";
    let start = Instant::now();
    loop {
        let id = crate::sweep::until_q(ctx, sel)?;
        let v = crate::sweep::attr(ctx, id, "aria-valuenow")?.unwrap_or_default();
        let n: usize = v.split(' ').next().and_then(|n| n.parse().ok()).unwrap_or(0);
        if n >= 3 && v.contains(" %") {
            break;
        }
        if start.elapsed() > Duration::from_secs(15) {
            return Err(format!("the CPU graph holds {v:?}, not 3 samples"));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    ctx.keep_shot("monitor")?;
    Ok(())
}

/// `@monitor.sweep`: every control Monitor shows, worked by its box and read
/// back from its tree and its door: each tab (selected, the others not);
/// each sort header (pressed, Monitor sorted by it, a second click the other
/// way); each fault row (opened, then closed); each log chip (the filter
/// moves on) (TASK-063, FR-040).
pub fn sweep(ctx: &mut Ctx) -> Result<(), String> {
    use crate::sweep::{attr, click, q, sweep, until_attr, until_q};
    ctx.expect_log("desktop: the Rack is up")?;
    ctx.door("monitor open")?;
    const MON: &str = "WINDOW[data-owner=\"monitor\"]";
    let w = until_q(ctx, MON)?;
    let wid = attr(ctx, w, "data-window-id")?.ok_or("no window id")?;
    ctx.cdp.call("Target.activateTarget", fjson::Value::obj([("targetId", wid.as_str().into())]))?;
    let tabs = sweep(ctx, MON, "TAB", |ctx, c| {
        until_attr(ctx, &c.sel, "aria-selected", "true")?;
        until(ctx, "the tab", |s| s.tab == c.label).map(|_| ())
    })?;
    if tabs != 5 {
        return Err(format!("{tabs} tabs, not 5"));
    }
    // Each tab's own controls.
    let tab = |ctx: &mut Ctx, id: &str| -> Result<(), String> {
        let t = until_q(ctx, &format!("{MON} [data-testid=\"tab-{id}\"]"))?;
        click(ctx, t)?;
        until_attr(ctx, &format!("{MON} [data-testid=\"tab-{id}\"]"), "aria-selected", "true")
    };
    tab(ctx, "processes")?;
    sweep(ctx, MON, "BUTTON", |ctx, c| {
        let col = match c.testid.as_str() {
            "sort-service" => "Name",
            "sort-state" => "State",
            "sort-memory" => "Memory",
            "sort-cpu" => "Cpu",
            "sort-restarts" => "Restarts",
            other => return Err(format!("an unknown header {other}")),
        };
        until_attr(ctx, &c.sel, "aria-pressed", "true")?;
        let s = until(ctx, "sorted", |s| s.sort.starts_with(col))?;
        // Again: the other way.
        let id = q(ctx, &c.sel)?;
        let dir = attr(ctx, id, "aria-valuenow")?.unwrap_or_default();
        click(ctx, id)?;
        let flipped = if dir == "ascending" { "descending" } else { "ascending" };
        until_attr(ctx, &c.sel, "aria-valuenow", flipped)?;
        until(ctx, "the other way", |n| n.sort.starts_with(col) && n.sort != s.sort).map(|_| ())
    })?;
    tab(ctx, "faults")?;
    if q(ctx, &format!("{MON} BUTTON"))? != 0 {
        sweep(ctx, MON, "BUTTON", |ctx, c| {
            until_attr(ctx, &c.sel, "aria-expanded", "true")?;
            let id = q(ctx, &c.sel)?;
            click(ctx, id)?;
            crate::sweep::until_gone(ctx, &format!("{}[aria-expanded=\"true\"]", c.sel))
        })?;
    }
    tab(ctx, "log")?;
    sweep(ctx, MON, "BUTTON", |ctx, c| {
        let was = c.label.clone();
        let s = until(ctx, "the filter moved on", |s| !s.filter.is_empty())?;
        let start = std::time::Instant::now();
        loop {
            let id = q(ctx, &c.sel)?;
            let now = attr(ctx, id, "aria-label")?.unwrap_or_default();
            if now != was {
                break;
            }
            if start.elapsed() > Duration::from_secs(10) {
                return Err(format!("{was} didn't change"));
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        let _ = s;
        Ok(())
    })?;
    // Closed (Alt+F4), so the next spec finds Monitor as it starts.
    tab(ctx, "processes")?;
    crate::stage::key(ctx, "AltLeft", true)?;
    crate::stage::key(ctx, "F4", true)?;
    crate::stage::key(ctx, "F4", false)?;
    crate::stage::key(ctx, "AltLeft", false)?;
    ctx.expect_log("monitor: closed")?;
    crate::sweep::until_gone(ctx, MON)
}

/// `@monitor.devices`: Monitor → Devices lists every CPU with its core type
/// and every device devmgr found, each with the driver that took it: the
/// firmware's framebuffer and the display controller holding it to
/// drv-gop-fb, the PS/2 controller to drv-i8042, COM2 and COM3 to
/// drv-uart16550, a device no driver took with "—". The wheel, the arrows,
/// Home and End scroll it, never past either end (TASK-075, TASK-070; an
/// 8-CPU VM, so the list is longer than the window).
pub fn devices(ctx: &mut Ctx) -> Result<(), String> {
    use crate::stage::wheel;
    ctx.expect_log("desktop: the Rack is up")?;
    ctx.door("monitor open")?;
    ctx.expect_log("monitor: open")?;
    let start = Instant::now();
    let f = loop {
        match frame(ctx) {
            Ok(f) => break f,
            Err(e) if start.elapsed() > Duration::from_secs(10) => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    };
    click(ctx, f, 16 + 560 + 30, 20)?;
    let s = until(ctx, "the Devices tab with every driver", |s| {
        s.tab == "Devices" && s.lines.iter().any(|l| l.contains("| drv-uart16550"))
    })?;
    let has = |want: &[&str]| s.lines.iter().any(|l| want.iter().all(|w| l.contains(w)));
    if !s.lines.first().is_some_and(|l| l.starts_with("8 CPUs") && l.contains("x2APIC")) {
        return Err(format!("the CPUs' line: {:?}", s.lines.first()));
    }
    for (want, what) in [
        (&["CPU 0 |", "| Freally Core"][..], "CPU 0"),
        (&["CPU 7 |", "| Freally Core"][..], "CPU 7"),
        (&["framebuffer |", "| drv-gop-fb"][..], "the framebuffer"),
        (&["Display (VGA)", "| drv-gop-fb"][..], "the display controller"),
        (&["PS/2 controller |", "Keyboard and mouse", "| drv-i8042"][..], "the PS/2 controller"),
        (&["COM2 |", "Serial port", "| drv-uart16550"][..], "COM2"),
        (&["COM3 |", "| drv-uart16550"][..], "COM3"),
        (&["Host bridge", "| —"][..], "a device no driver took"),
    ] {
        if !has(want) {
            return Err(format!("{what} isn't listed as it should be: {:?}", s.lines));
        }
    }
    // Scrolling: the wheel down a notch, End, never past the
    // last row, Home, never above the first.
    let top = |s: &State| s.lines.last().and_then(|l| l.strip_prefix("top ")).and_then(|v| v.parse::<i64>().ok());
    move_to(ctx, f.0 + 400, f.1 + 32 + 300)?;
    wheel(ctx, false)?;
    let notch = until(ctx, "a notch down", |s| top(s).is_some_and(|t| t > 0))?;
    let after_notch = top(&notch).unwrap_or(0);
    key(ctx, "End", true)?;
    key(ctx, "End", false)?;
    let end = until(ctx, "End", |s| top(s).is_some_and(|t| t >= after_notch))?;
    let last = top(&end).unwrap_or(0);
    key(ctx, "ArrowDown", true)?;
    key(ctx, "ArrowDown", false)?;
    std::thread::sleep(Duration::from_millis(400));
    if top(&state(ctx)?) != Some(last) {
        return Err(String::from("the list scrolled past its last row"));
    }
    key(ctx, "Home", true)?;
    key(ctx, "Home", false)?;
    until(ctx, "Home", |s| top(s) == Some(0))?;
    wheel(ctx, true)?;
    std::thread::sleep(Duration::from_millis(400));
    if top(&state(ctx)?) != Some(0) {
        return Err(String::from("the list scrolled above its first row"));
    }
    ctx.keep_shot("monitor-devices")?;
    // Closed (Alt+F4), so the next spec finds Monitor as it starts.
    key(ctx, "AltLeft", true)?;
    key(ctx, "F4", true)?;
    key(ctx, "F4", false)?;
    key(ctx, "AltLeft", false)?;
    ctx.expect_log("monitor: closed")
}

/// `@monitor.last_panic`: the kernel panics (the e2e door's `kernel panic`,
/// syscall 38, e2e kernels only); after a warm reboot Monitor → Faults lists
/// it from the last-panic page: freally-core, Panic, the last boot, with
/// what the panic said (TASK-074).
pub fn last_panic(ctx: &mut Ctx) -> Result<(), String> {
    ctx.allow_errors(&["a test panic, asked for through the e2e test door"]);
    ctx.allow_faults();
    ctx.expect_log("desktop: the Rack is up")?;
    // No answer: the machine stops.
    let _ = ctx.door("kernel panic");
    ctx.expect_log("Freally Core panicked: a test panic")?;
    ctx.reset()?;
    ctx.expect_log("freally-core: the last boot stopped: Freally Core panicked: a test panic")?;
    ctx.expect_log_count("desktop: the Rack is up", 2)?;
    ctx.door("monitor open")?;
    ctx.expect_log("monitor: open")?;
    let start = Instant::now();
    let f = loop {
        match frame(ctx) {
            Ok(f) => break f,
            Err(e) if start.elapsed() > Duration::from_secs(10) => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    };
    click(ctx, f, 16 + 280 + 30, 20)?;
    let s = until(ctx, "the last boot's panic in Faults", |s| {
        s.tab == "Faults" && s.lines.iter().any(|l| l.contains("freally-core Panic (the last boot)"))
    })?;
    // Opened: what it said, and that the last boot stopped there.
    click(ctx, f, 40, 66)?;
    until(ctx, "the panic's details", |s| {
        s.lines.iter().any(|l| l.contains("the last boot stopped here"))
            && s.lines.iter().any(|l| l.contains("a test panic, asked for through the e2e test door"))
    })?;
    let _ = s;
    ctx.keep_shot("monitor-last-panic")?;
    key(ctx, "AltLeft", true)?;
    key(ctx, "F4", true)?;
    key(ctx, "F4", false)?;
    key(ctx, "AltLeft", false)?;
    ctx.expect_log("monitor: closed")
}
