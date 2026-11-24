// SPDX-License-Identifier: Apache-2.0
//! Settings (TASK-104, FR-048), on a GPT disk with an Owner logged in:
//! every control of Personalisation worked and read back from stage
//! (`Freally.readBack stage look`); a change isn't stored until Apply, and
//! Cancel, Escape and the window's Cancel put back what was shown live; OK
//! saves and closes; the look comes back at the next login; Restore
//! Defaults; the keyboard (the tab list one stop, its arrows, each row one
//! stop); and Accounts: add, change the password and back, export, import
//! (from the list, renamed), remove, each through its dialog.

use crate::Ctx;
use crate::session::{expect_error, expect_focus, expect_value, press, sel, typed, waits_for_log};
use crate::stage::key;
use crate::sweep::{attr, click, q, until_attr, until_gone, until_q};
use fjson::Value;
use std::time::{Duration, Instant};

const OWNER: &str = "mike";
const PASSWORD: &str = "correct-horse";
const DEFAULTS: &str = "dark scale=0 wallpaper=studio motion=off clock=12 saver=0 idle=10";

fn ok(ctx: &Ctx, cmd: &str) -> Result<(), String> {
    let said = ctx.door(cmd)?;
    if said != ["ok"] {
        return Err(format!("{cmd}: {said:?}"));
    }
    Ok(())
}

/// The Owner made (on a new disk) and logged in, and the desktop theirs.
pub(crate) fn signed_in(ctx: &mut Ctx) -> Result<(), String> {
    if ctx.door("accounts who")? == [OWNER] {
        return Ok(());
    }
    if ctx.door("accounts list")? == ["(none)"] {
        ok(ctx, &format!("accounts create {OWNER} {PASSWORD}"))?;
    }
    let made = ctx.log_count("desktop: mike's session");
    ok(ctx, &format!("accounts login {OWNER} {PASSWORD}"))?;
    waits_for_log(ctx, "desktop: mike's session", made)?;
    Ok(())
}

/// What stage shows now, in cdpd's words.
fn look(ctx: &mut Ctx) -> Result<String, String> {
    let v = ctx
        .cdp
        .call("Freally.readBack", Value::obj([("service", "stage".into()), ("query", "look".into())]))
        .map_err(|e| format!("Freally.readBack: {e}"))?;
    Ok(v.get("value").and_then(Value::as_str).unwrap_or("").to_string())
}

/// Waits (10 s) for stage's look to hold every word of `want`.
fn expect_look(ctx: &mut Ctx, want: &str) -> Result<(), String> {
    let start = Instant::now();
    loop {
        let now = look(ctx)?;
        let words: Vec<&str> = now.split(' ').collect();
        if want.split(' ').all(|w| words.contains(&w)) {
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err(format!("stage's look is {now:?}, not {want:?}"));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

pub(crate) fn press_id(ctx: &mut Ctx, id: &str) -> Result<(), String> {
    let n = until_q(ctx, &sel(id))?;
    click(ctx, n)
}

fn dark(ctx: &mut Ctx, id: &str) -> Result<bool, String> {
    let n = until_q(ctx, &sel(id))?;
    Ok(attr(ctx, n, "aria-disabled")?.as_deref() == Some("true"))
}

/// Waits (10 s) for key `id` to be lit (`true`) or dark.
fn expect_lit(ctx: &mut Ctx, id: &str, lit: bool) -> Result<(), String> {
    let start = Instant::now();
    while dark(ctx, id)? == lit {
        if start.elapsed() > Duration::from_secs(10) {
            return Err(format!("{id} is {}", if lit { "dark" } else { "lit" }));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    Ok(())
}

pub(crate) fn chosen(ctx: &mut Ctx, id: &str) -> Result<(), String> {
    until_attr(ctx, &sel(id), "aria-selected", "true")
}

fn with(ctx: &mut Ctx, held: &str, code: &str) -> Result<(), String> {
    key(ctx, held, true)?;
    press(ctx, code)?;
    key(ctx, held, false)
}

/// Settings open (Super+I, as Windows has it), on Personalisation.
pub(crate) fn open(ctx: &mut Ctx) -> Result<(), String> {
    if q(ctx, &sel("settings-tabs"))? == 0 {
        with(ctx, "MetaLeft", "KeyI")?;
        until_q(ctx, &sel("settings-tabs"))?;
    }
    press_id(ctx, "settings-tab-personalisation")?;
    chosen(ctx, "settings-tab-personalisation")
}

fn closed(ctx: &mut Ctx) -> Result<(), String> {
    until_gone(ctx, &sel("settings-tabs"))
}

/// The defaults, saved (whatever an earlier spec left).
fn from_defaults(ctx: &mut Ctx) -> Result<(), String> {
    open(ctx)?;
    if !dark(ctx, "settings-defaults")? {
        press_id(ctx, "settings-defaults")?;
    }
    if !dark(ctx, "settings-apply")? {
        press_id(ctx, "settings-apply")?;
    }
    expect_look(ctx, DEFAULTS)?;
    expect_lit(ctx, "settings-apply", false)
}

/// `@settings.look`: every Personalisation control worked and read back;
/// the theme and background live, the rest only at Apply; Cancel, Escape
/// put back what was live; OK saves and closes; the look kept in the
/// account comes back at the next login; Restore Defaults.
pub fn look_spec(ctx: &mut Ctx) -> Result<(), String> {
    signed_in(ctx)?;
    from_defaults(ctx)?;
    // The theme: live, and Apply lights, and the panel carries the dot.
    press_id(ctx, "settings-theme-1")?;
    chosen(ctx, "settings-theme-1")?;
    expect_look(ctx, "light")?;
    expect_lit(ctx, "settings-apply", true)?;
    until_attr(ctx, &sel("settings-tab-personalisation"), "aria-valuenow", "changed")?;
    // Every background, live, each one rendered at full size.
    for (k, name) in ["studio", "circuit", "orbit", "connected"].into_iter().enumerate() {
        let id = format!("settings-wallpaper-{k}");
        until_attr(ctx, &sel(&id), "aria-valuenow", "rendered")?;
        press_id(ctx, &id)?;
        chosen(ctx, &id)?;
        expect_look(ctx, &format!("wallpaper={name}"))?;
    }
    ctx.keep_shot("settings-personalisation-light")?;
    // The clock waits for Apply.
    press_id(ctx, "settings-clock-1")?;
    chosen(ctx, "settings-clock-1")?;
    std::thread::sleep(Duration::from_millis(600));
    expect_look(ctx, "clock=12")?;
    press_id(ctx, "settings-apply")?;
    expect_look(ctx, "light wallpaper=connected clock=24")?;
    expect_lit(ctx, "settings-apply", false)?;
    // Every screensaver; none turns the idle time off.
    for k in 0..5 {
        let id = format!("settings-saver-{k}");
        press_id(ctx, &id)?;
        chosen(ctx, &id)?;
    }
    until_attr(ctx, &sel("settings-idle"), "aria-disabled", "true")?;
    press_id(ctx, "settings-saver-3")?;
    until_attr(ctx, &sel("settings-idle"), "aria-valuenow", "10 minutes")?;
    press_id(ctx, "settings-idle-up")?;
    until_attr(ctx, &sel("settings-idle"), "aria-valuenow", "15 minutes")?;
    press_id(ctx, "settings-idle-down")?;
    press_id(ctx, "settings-idle-down")?;
    until_attr(ctx, &sel("settings-idle"), "aria-valuenow", "5 minutes")?;
    press_id(ctx, "settings-idle-up")?;
    press_id(ctx, "settings-idle-up")?;
    until_attr(ctx, &sel("settings-idle"), "aria-valuenow", "15 minutes")?;
    // Reduce Motion, both ways and on.
    for want in ["true", "false", "true"] {
        press_id(ctx, "settings-motion")?;
        until_attr(ctx, &sel("settings-motion"), "aria-checked", want)?;
    }
    // The scale: Automatic, then 100 %, 125 %.
    until_attr(ctx, &sel("settings-scale"), "aria-valuenow", "Automatic")?;
    press_id(ctx, "settings-scale-up")?;
    press_id(ctx, "settings-scale-up")?;
    until_attr(ctx, &sel("settings-scale"), "aria-valuenow", "125 %")?;
    expect_look(ctx, "scale=0 motion=off saver=0")?;
    press_id(ctx, "settings-apply")?;
    expect_look(ctx, "scale=125 motion=on saver=3 idle=15")?;
    // Made again at 125 %, still Settings.
    until_q(ctx, &sel("settings-tabs"))?;
    ctx.keep_shot("settings-at-125")?;
    press_id(ctx, "settings-scale-down")?;
    press_id(ctx, "settings-scale-down")?;
    until_attr(ctx, &sel("settings-scale"), "aria-valuenow", "Automatic")?;
    press_id(ctx, "settings-apply")?;
    expect_look(ctx, "scale=0")?;
    until_q(ctx, &sel("settings-tabs"))?;
    // Escape is Cancel: the live theme goes back, and nothing is stored.
    press_id(ctx, "settings-theme-0")?;
    expect_look(ctx, "dark")?;
    press(ctx, "Escape")?;
    closed(ctx)?;
    expect_look(ctx, "light")?;
    // The bar's Cancel, the same.
    open(ctx)?;
    chosen(ctx, "settings-theme-1")?;
    press_id(ctx, "settings-wallpaper-2")?;
    expect_look(ctx, "wallpaper=orbit")?;
    press_id(ctx, "settings-cancel")?;
    closed(ctx)?;
    expect_look(ctx, "wallpaper=connected")?;
    // A click outside (another window takes the focus): Cancel too.
    open(ctx)?;
    press_id(ctx, "settings-wallpaper-1")?;
    expect_look(ctx, "wallpaper=circuit")?;
    let said = ctx.door("stage open 240 120 203040 Elsewhere")?;
    if !said.iter().any(|l| l.starts_with("window ")) {
        return Err(format!("stage open: {said:?}"));
    }
    closed(ctx)?;
    expect_look(ctx, "wallpaper=connected")?;
    with(ctx, "AltLeft", "F4")?;
    // OK saves and closes.
    open(ctx)?;
    press_id(ctx, "settings-clock-0")?;
    press_id(ctx, "settings-ok")?;
    closed(ctx)?;
    let saved = "light scale=0 wallpaper=connected motion=on clock=12 saver=3 idle=15";
    expect_look(ctx, saved)?;
    // Kept in the account: logged out, the screen changed, and back.
    let shown = ctx.log_count("login: shown");
    ok(ctx, "accounts logout")?;
    waits_for_log(ctx, "login: shown", shown)?;
    ok(ctx, "stage theme dark")?;
    expect_look(ctx, "dark")?;
    let made = ctx.log_count("desktop: mike's session");
    ok(ctx, &format!("accounts login {OWNER} {PASSWORD}"))?;
    waits_for_log(ctx, "desktop: mike's session", made)?;
    expect_look(ctx, saved)?;
    // Restore Defaults resets the panel; Apply keeps them.
    open(ctx)?;
    press_id(ctx, "settings-defaults")?;
    chosen(ctx, "settings-theme-0")?;
    expect_look(ctx, "dark wallpaper=studio")?;
    expect_lit(ctx, "settings-defaults", false)?;
    press_id(ctx, "settings-ok")?;
    closed(ctx)?;
    expect_look(ctx, DEFAULTS)
}

/// `@settings.keyboard`: the tab list is one stop and its arrows cycle
/// (Home, End); each row is one stop, worked by its keys (the scale's and
/// the idle time's steppers one step each way); Tab reaches OK
/// and Cancel and comes round; Shift+Tab goes back; Escape cancels.
pub fn keyboard(ctx: &mut Ctx) -> Result<(), String> {
    signed_in(ctx)?;
    from_defaults(ctx)?;
    // A click on a tab puts the focus on the tab list.
    press_id(ctx, "settings-tab-personalisation")?;
    expect_focus(ctx, "settings-tab-personalisation")?;
    for (code, id) in [
        ("ArrowDown", "settings-tab-accounts"),
        ("ArrowDown", "settings-tab-time"),
        ("ArrowDown", "settings-tab-update"),
        ("ArrowDown", "settings-tab-hardware"),
        ("ArrowDown", "settings-tab-about"),
        ("ArrowDown", "settings-tab-network"),
        ("ArrowDown", "settings-tab-personalisation"),
        ("ArrowUp", "settings-tab-network"),
        ("ArrowUp", "settings-tab-about"),
        ("Home", "settings-tab-network"),
        ("End", "settings-tab-about"),
        ("Home", "settings-tab-network"),
        ("ArrowDown", "settings-tab-personalisation"),
    ] {
        press(ctx, code)?;
        chosen(ctx, id)?;
    }
    press(ctx, "Tab")?;
    expect_focus(ctx, "settings-theme")?;
    press(ctx, "ArrowRight")?;
    expect_look(ctx, "light")?;
    press(ctx, "ArrowRight")?;
    expect_look(ctx, "dark")?;
    for id in ["settings-wallpaper", "settings-scale"] {
        press(ctx, "Tab")?;
        expect_focus(ctx, id)?;
    }
    // A stepper's keys: one step each way.
    press(ctx, "ArrowRight")?;
    until_attr(ctx, &sel("settings-scale"), "aria-valuenow", "100 %")?;
    press(ctx, "ArrowLeft")?;
    until_attr(ctx, &sel("settings-scale"), "aria-valuenow", "Automatic")?;
    press(ctx, "Tab")?;
    expect_focus(ctx, "settings-motion")?;
    press(ctx, "Space")?;
    until_attr(ctx, &sel("settings-motion"), "aria-checked", "true")?;
    press(ctx, "Space")?;
    until_attr(ctx, &sel("settings-motion"), "aria-checked", "false")?;
    for id in ["settings-clock", "settings-saver", "settings-idle"] {
        press(ctx, "Tab")?;
        expect_focus(ctx, id)?;
    }
    press(ctx, "ArrowRight")?;
    until_attr(ctx, &sel("settings-idle"), "aria-valuenow", "15 minutes")?;
    press(ctx, "ArrowLeft")?;
    until_attr(ctx, &sel("settings-idle"), "aria-valuenow", "10 minutes")?;
    for id in ["settings-ok", "settings-cancel"] {
        press(ctx, "Tab")?;
        expect_focus(ctx, id)?;
    }
    press(ctx, "Tab")?;
    expect_focus(ctx, "settings-tab-personalisation")?;
    key(ctx, "ShiftLeft", true)?;
    press(ctx, "Tab")?;
    key(ctx, "ShiftLeft", false)?;
    expect_focus(ctx, "settings-cancel")?;
    press(ctx, "Escape")?;
    closed(ctx)?;
    expect_look(ctx, DEFAULTS)
}

/// The host's zone, as QEMU's `-rtc base=localtime` keeps its clock:
/// minutes east of UTC (`date +%z`, the runner's own environment).
pub(crate) fn host_zone() -> Result<i16, String> {
    let out = std::process::Command::new("date").arg("+%z").output().map_err(|e| format!("date: {e}"))?;
    let z = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let num = |r: std::ops::Range<usize>| z.get(r).and_then(|v| v.parse::<i16>().ok());
    match (z.get(..1), num(1..3), num(3..5)) {
        (Some("+"), Some(h), Some(m)) if z.len() == 5 => Ok(h * 60 + m),
        (Some("-"), Some(h), Some(m)) if z.len() == 5 => Ok(-(h * 60 + m)),
        _ => Err(format!("date +%z said {z:?}")),
    }
}

/// The civil date and time (UTC) of Unix seconds: year, month, day, hour,
/// minute, second (Howard Hinnant's days-to-civil).
pub(crate) fn civil(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let (days, s) = ((secs / 86_400) as i64, secs % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, (s / 3600) as u32, (s / 60 % 60) as u32, (s % 60) as u32)
}

/// Now, in Unix seconds, by the host.
pub(crate) fn host_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// A zone as Settings names it: `UTC`, `UTC−06:00`, `UTC+05:30`.
fn zone_name(m: i16) -> String {
    if m == 0 {
        return "UTC".into();
    }
    let a = m.unsigned_abs();
    format!("UTC{}{:02}:{:02}", if m < 0 { '\u{2212}' } else { '+' }, a / 60, a % 60)
}

/// What Settings says of a time (`18:05 UTC, 24 Nov 2026`), for each minute
/// from a minute before `secs` to a minute after (the panel was drawn a
/// moment ago, and a minute may turn).
fn said_near(secs: u64, zone: &str) -> Vec<String> {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    [secs - 60, secs, secs + 60]
        .into_iter()
        .map(|t| {
            let (y, mo, d, h, mi, _) = civil(t);
            format!("{h:02}:{mi:02} {zone}, {d} {} {y}", MONTHS[mo as usize - 1])
        })
        .collect()
}

/// Waits (10 s) for the line `id` (`prefix` and a time) to say a time
/// `said_near` gives.
fn expect_time(ctx: &mut Ctx, id: &str, prefix: &str, secs: u64, zone: &str) -> Result<(), String> {
    let want: Vec<String> = said_near(secs, zone).into_iter().map(|w| format!("{prefix}{w}")).collect();
    let start = Instant::now();
    loop {
        let n = until_q(ctx, &sel(id))?;
        let said = attr(ctx, n, "aria-label")?.unwrap_or_default();
        if want.contains(&said) {
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err(format!("{id} says {said:?}, not one of {want:?}"));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// Settings open on Time and language.
fn time_panel(ctx: &mut Ctx) -> Result<(), String> {
    open(ctx)?;
    press_id(ctx, "settings-tab-time")?;
    chosen(ctx, "settings-tab-time")
}

/// `@settings.time` (TASK-180A): Time and language. QEMU keeps its RTC on
/// the host's local time, as Windows keeps a PC's: until the zone is set,
/// the true time Freally OS knows is wrong by the host's offset. The zone
/// stepped to the host's (both stepper keys worked) and *Local time* chosen;
/// Escape puts a change back; Apply saves it through conductor (its log),
/// and the true time is then the host's UTC and the time here its local
/// time; the zone is kept over a restart (the kernel reads `FreallyClock`
/// at boot, its log); Restore Defaults puts UTC back.
pub fn time(ctx: &mut Ctx) -> Result<(), String> {
    signed_in(ctx)?;
    time_panel(ctx)?;
    // Whatever an earlier run left: the defaults, saved.
    if !dark(ctx, "settings-defaults")? {
        press_id(ctx, "settings-defaults")?;
    }
    if !dark(ctx, "settings-apply")? {
        press_id(ctx, "settings-apply")?;
    }
    expect_value(ctx, "settings-zone", "UTC")?;
    chosen(ctx, "settings-rtc-1")?;
    // Each row one stop, worked by its keys.
    press_id(ctx, "settings-tab-time")?;
    press(ctx, "Tab")?;
    expect_focus(ctx, "settings-zone")?;
    press(ctx, "ArrowRight")?;
    expect_value(ctx, "settings-zone", "UTC+01:00")?;
    press(ctx, "ArrowLeft")?;
    expect_value(ctx, "settings-zone", "UTC")?;
    press(ctx, "Tab")?;
    expect_focus(ctx, "settings-rtc")?;
    press(ctx, "ArrowLeft")?;
    chosen(ctx, "settings-rtc-0")?;
    press(ctx, "ArrowRight")?;
    chosen(ctx, "settings-rtc-1")?;
    expect_lit(ctx, "settings-apply", false)?;
    let off = host_zone()?;
    let want = zone_name(off);
    ctx.note(&format!("the host's zone: {want}"));
    // The RTC read as UTC: the "true" time is the host's local time.
    let local = host_now().saturating_add_signed(off as i64 * 60);
    expect_time(ctx, "settings-time-utc", "The true time: ", local, "UTC")?;
    // Both stepper keys, then Escape: nothing kept.
    press_id(ctx, "settings-zone-up")?;
    expect_value(ctx, "settings-zone", "UTC+01:00")?;
    press_id(ctx, "settings-zone-down")?;
    press_id(ctx, "settings-zone-down")?;
    expect_value(ctx, "settings-zone", "UTC\u{2212}01:00")?;
    until_attr(ctx, &sel("settings-tab-time"), "aria-valuenow", "changed")?;
    press(ctx, "Escape")?;
    closed(ctx)?;
    time_panel(ctx)?;
    expect_value(ctx, "settings-zone", "UTC")?;
    // The host's zone, a step at a time.
    for _ in 0..40 {
        let now = crate::session::value(ctx, "settings-zone")?;
        if now == want {
            break;
        }
        let east = off > 0;
        press_id(ctx, if east { "settings-zone-up" } else { "settings-zone-down" })?;
        let start = Instant::now();
        while crate::session::value(ctx, "settings-zone")? == now {
            if start.elapsed() > Duration::from_secs(5) {
                return Err(format!("the zone stayed {now:?} after a step"));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    expect_value(ctx, "settings-zone", &want)?;
    press_id(ctx, "settings-rtc-1")?;
    press_id(ctx, "settings-rtc-0")?;
    chosen(ctx, "settings-rtc-0")?;
    let line = format!("conductor: the zone is {want}; the RTC keeps local time");
    let saved = ctx.log_count(&line);
    press_id(ctx, "settings-apply")?;
    waits_for_log(ctx, &line, saved)?;
    expect_lit(ctx, "settings-apply", false)?;
    let now = host_now();
    expect_time(ctx, "settings-time-utc", "The true time: ", now, "UTC")?;
    expect_time(ctx, "settings-time-here", "The time here: ", now.saturating_add_signed(off as i64 * 60), &want)?;
    // Kept over a restart: the kernel reads it at boot.
    press_id(ctx, "settings-ok")?;
    closed(ctx)?;
    crate::session::to_login(ctx)?;
    press(ctx, "Tab")?;
    press(ctx, "Tab")?;
    expect_focus(ctx, "login-restart")?;
    let (boots, shown) = (ctx.log_count("freally-boot "), ctx.log_count("login: shown"));
    ctx.rebooted_by_guest();
    press(ctx, "Enter")?;
    waits_for_log(ctx, "freally-boot ", boots)?;
    waits_for_log(ctx, "login: shown", shown)?;
    let tail = ctx.log();
    let since = tail.rfind("freally-boot ").map_or("", |i| &tail[i..]);
    let line = format!("freally-core: the zone is {want}; the RTC keeps local time");
    if !since.contains(&line) {
        return Err(format!("after the restart the kernel didn't say {line:?}"));
    }
    signed_in(ctx)?;
    time_panel(ctx)?;
    expect_value(ctx, "settings-zone", &want)?;
    chosen(ctx, "settings-rtc-0")?;
    expect_time(ctx, "settings-time-utc", "The true time: ", host_now(), "UTC")?;
    // Restore Defaults: UTC, the RTC read as UTC.
    let line = "conductor: the zone is UTC; the RTC keeps UTC";
    let saved = ctx.log_count(line);
    press_id(ctx, "settings-defaults")?;
    expect_value(ctx, "settings-zone", "UTC")?;
    chosen(ctx, "settings-rtc-1")?;
    press_id(ctx, "settings-ok")?;
    waits_for_log(ctx, line, saved)?;
    closed(ctx)
}

/// A line of Settings ▸ Network, as the tree holds it.
fn network_line(ctx: &mut Ctx, id: &str) -> Result<String, String> {
    let n = until_q(ctx, &sel(id))?;
    Ok(attr(ctx, n, "aria-label")?.unwrap_or_default())
}

/// `@settings.network` (TASK-183): Settings ▸ Network (a guest's, on the
/// VM's own network): the interface as its driver and netstack's DHCP lease
/// have it (driver, MAC, link; address, router, DNS), every connection the
/// computer can make (netstack's list, a test image's nine), and none open.
pub fn network(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("netstack: 10.0.2.15/24 from 10.0.2.2 by DHCP")?;
    // The lease as netstack said it: QEMU's restricted network gives no router or DNS.
    let log = ctx.log();
    let lease = log.lines().find(|l| l.contains("by DHCP, router ")).ok_or("no lease line")?;
    let part = |after: &str, until: &str| {
        lease.split(after).nth(1).and_then(|r| r.split(until).next()).map(String::from).ok_or(format!("{lease:?}"))
    };
    let (router, dns) = (part("router ", ",")?, part("DNS ", ", lease")?);
    let info = ctx.door("netdev info")?;
    let first = info.first().cloned().unwrap_or_default();
    let mac = first.split("mac=").nth(1).and_then(|r| r.split(' ').next()).ok_or(format!("netdev info: {info:?}"))?;
    let driver = first.split_whitespace().nth(1).ok_or(format!("netdev info: {info:?}"))?;
    open(ctx)?;
    press_id(ctx, "settings-tab-network")?;
    chosen(ctx, "settings-tab-network")?;
    let want = [
        ("settings-network-iface-0", format!("eth0 · {driver} · {mac} · connected")),
        ("settings-network-ipv4-0", format!("IPv4 10.0.2.15/24 · router {router} · DNS {dns}")),
        (
            "settings-network-grant-0",
            String::from(
                "netstack: DHCP on each network (UDP 68 to 67): an address, a router and DNS servers; when the network comes up, and to renew its lease.",
            ),
        ),
        (
            "settings-network-grant-3",
            String::from("terminal: what you type: ip, ping, nslookup and fetch; only when you run them."),
        ),
        (
            "settings-network-grant-6",
            String::from(
                "updater: the update host (HTTPS), for its list of updates and then the one you choose; only when you press Check online, then Download.",
            ),
        ),
        (
            "settings-network-grant-5",
            String::from("monitor: its Network tab: each network's packets and bytes; while that tab is shown."),
        ),
        (
            "settings-network-grant-8",
            String::from("cdpd: the test harness's view of the network; in test images only."),
        ),
        ("settings-network-open-0", String::from("No connection is open.")),
    ];
    for (id, words) in want {
        let said = network_line(ctx, id)?;
        if said != words {
            return Err(format!("{id} says {said:?}, not {words:?}"));
        }
    }
    if q(ctx, &sel("settings-network-grant-9"))? != 0 {
        return Err("more than the nine connections a test image can make".into());
    }
    press(ctx, "Escape")?;
    closed(ctx)
}

/// One dialog's key pressed, and its words once it's done.
fn done(ctx: &mut Ctx, want: &str) -> Result<(), String> {
    press(ctx, "Enter")?;
    // Argon2id, and an export or an import: give it time.
    let start = Instant::now();
    loop {
        let n = q(ctx, &sel("settings-dialog-done"))?;
        if n != 0 {
            let said = attr(ctx, n, "aria-valuenow")?.unwrap_or_default();
            if !said.starts_with(want) {
                return Err(format!("the dialog said {said:?}, not {want:?}…"));
            }
            break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            return Err(format!("the dialog never said {want:?}"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    press(ctx, "Enter")?;
    until_gone(ctx, &sel("settings-dialog-done"))
}

fn backspace(ctx: &mut Ctx, n: usize) -> Result<(), String> {
    (0..n).try_for_each(|_| press(ctx, "Backspace"))
}

fn listed(ctx: &Ctx, user: &str) -> Result<bool, String> {
    Ok(ctx.door("accounts list")?.iter().any(|l| l.split(' ').next() == Some(user)))
}

/// `@settings.accounts`: Accounts lists who's here; *Add an account*
/// (cancelled with nothing made; a mismatch said under its field, then made); *Change your password* (a
/// wrong one said, then changed and back); *Export*; *Import* from the list
/// (opened with its password, previewed, renamed); *Remove* (the username
/// typed exactly), and the Owner has no Remove.
pub fn accounts(ctx: &mut Ctx) -> Result<(), String> {
    signed_in(ctx)?;
    open(ctx)?;
    press_id(ctx, "settings-tab-accounts")?;
    until_attr(ctx, &sel("settings-account-mike"), "aria-valuenow", "mike · Owner · you")?;
    if q(ctx, &sel("settings-remove-mike"))? != 0 {
        return Err("the Owner's own account offers Remove".into());
    }
    // Add an account: cancelled first (nothing made), then made.
    press_id(ctx, "settings-account-add")?;
    until_q(ctx, &sel("settings-add-dialog"))?;
    typed(ctx, "kept")?;
    press_id(ctx, "settings-dialog-cancel")?;
    until_gone(ctx, &sel("settings-add-dialog"))?;
    if listed(ctx, "kept")? {
        return Err("a cancelled Add made the account".into());
    }
    press_id(ctx, "settings-account-add")?;
    until_q(ctx, &sel("settings-add-dialog"))?;
    expect_focus(ctx, "settings-add-username")?;
    for (text, last) in [("sam", false), ("Sam", false), ("sam-pass-1", false), ("sam-pass-2", false), (PASSWORD, true)]
    {
        typed(ctx, text)?;
        if !last {
            press(ctx, "Tab")?;
        }
    }
    press(ctx, "Enter")?;
    expect_error(ctx, "settings-add-again", "The two passwords aren't the same.")?;
    backspace(ctx, 1)?;
    typed(ctx, "1")?;
    done(ctx, "sam can log in now.")?;
    until_q(ctx, &sel("settings-account-sam"))?;
    if !listed(ctx, "sam")? {
        return Err("accounts doesn't list sam".into());
    }
    // Change your password: a wrong one said under it, then changed and back.
    press_id(ctx, "settings-account-password")?;
    typed(ctx, "wrong-password")?;
    press(ctx, "Tab")?;
    typed(ctx, "new-horse-1")?;
    press(ctx, "Tab")?;
    typed(ctx, "new-horse-1")?;
    press(ctx, "Enter")?;
    expect_error(ctx, "settings-password-current", "That password didn't match.")?;
    typed(ctx, PASSWORD)?;
    done(ctx, "Your password is changed.")?;
    press_id(ctx, "settings-account-password")?;
    for (text, last) in [("new-horse-1", false), (PASSWORD, false), (PASSWORD, true)] {
        typed(ctx, text)?;
        if !last {
            press(ctx, "Tab")?;
        }
    }
    done(ctx, "Your password is changed.")?;
    // Export.
    press_id(ctx, "settings-account-export")?;
    typed(ctx, PASSWORD)?;
    done(ctx, "Saved as /home/mike/Exports/mike.facct")?;
    // Import it again as another account, from the list.
    press_id(ctx, "settings-account-import")?;
    let file = "settings-import-file-mike.facct";
    let n = until_q(ctx, &sel(file))?;
    click(ctx, n)?;
    chosen(ctx, file)?;
    press_id(ctx, "settings-import-password")?;
    typed(ctx, PASSWORD)?;
    press(ctx, "Enter")?;
    let p = until_q(ctx, &sel("settings-import-preview"))?;
    let preview = attr(ctx, p, "aria-valuenow")?.unwrap_or_default();
    if !preview.contains("'mike'") {
        return Err(format!("the preview says {preview:?}"));
    }
    expect_focus(ctx, "settings-import-rename")?;
    typed(ctx, "mikey")?;
    press(ctx, "Tab")?;
    typed(ctx, PASSWORD)?;
    done(ctx, "mikey is imported")?;
    until_q(ctx, &sel("settings-account-mikey"))?;
    // Remove both: the username typed exactly.
    for user in ["mikey", "sam"] {
        press_id(ctx, &format!("settings-remove-{user}"))?;
        until_q(ctx, &sel("settings-remove-dialog"))?;
        typed(ctx, "nobody")?;
        press(ctx, "Tab")?;
        typed(ctx, PASSWORD)?;
        press(ctx, "Enter")?;
        expect_error(ctx, "settings-remove-confirm", "Type the username exactly as it's shown.")?;
        backspace(ctx, 6)?;
        typed(ctx, user)?;
        done(ctx, &format!("{user} is removed."))?;
        until_gone(ctx, &sel(&format!("settings-account-{user}")))?;
        if listed(ctx, user)? {
            return Err(format!("accounts still lists {user}"));
        }
    }
    ctx.keep_shot("settings-accounts")?;
    press(ctx, "Escape")?;
    closed(ctx)
}

/// What stage's clipboard holds (cdpd's read-back).
pub(crate) fn clipboard(ctx: &mut Ctx) -> Result<String, String> {
    let v = ctx
        .cdp
        .call("Freally.readBack", Value::obj([("service", "stage".into()), ("query", "clipboard".into())]))
        .map_err(|e| format!("Freally.readBack: {e}"))?;
    Ok(v.get("value").and_then(Value::as_str).unwrap_or("").to_string())
}

/// `@settings.hardware` (TASK-111, FR-053): the hardware report copied
/// (read back from stage's clipboard: every section, the driver that took
/// the NVMe disk, the image's version) and saved to Home ▸ Exports (read
/// back through the Terminal).
pub fn hardware(ctx: &mut Ctx) -> Result<(), String> {
    signed_in(ctx)?;
    open(ctx)?;
    press_id(ctx, "settings-tab-hardware")?;
    chosen(ctx, "settings-tab-hardware")?;
    let copied = ctx.log_count("settings: the hardware report copied");
    press_id(ctx, "settings-report-copy")?;
    ctx.expect_log_count("settings: the hardware report copied", copied + 1)?;
    let report = clipboard(ctx)?;
    let mut want: Vec<String> =
        ["Freally OS", "Processor", "Memory", "Firmware", "Display", "PCI devices", "Drivers", "Boot"]
            .iter()
            .map(|s| format!("\n{s}\n"))
            .collect();
    want.extend(
        ["Freally OS hardware report", "Nothing in it is sent anywhere", ": drv-nvme", "MiB at boot", "Features: "]
            .iter()
            .map(|s| s.to_string()),
    );
    for w in &want {
        if !report.contains(w.as_str()) {
            return Err(format!("the report has no {w:?}:\n{report}"));
        }
    }
    expect_value(ctx, "settings-report-note", "Copied: paste it into an email, or anywhere else.")?;
    let saved = ctx.log_count("settings: the hardware report saved to /home/mike/Exports/hardware-report.txt");
    press_id(ctx, "settings-report-save")?;
    ctx.expect_log_count("settings: the hardware report saved to /home/mike/Exports/hardware-report.txt", saved + 1)?;
    expect_value(ctx, "settings-report-note", "Saved to Home ▸ Exports ▸ hardware-report.txt.")?;
    press_id(ctx, "settings-cancel")?;
    closed(ctx)?;
    crate::terminal::open(ctx)?;
    crate::terminal::expect(
        ctx,
        "cat /home/mike/Exports/hardware-report.txt",
        // The report is longer than the Terminal: its tail is on the screen.
        &["Drivers", ": drv-nvme", "Boot"],
    )?;
    // The report stays on the clipboard until the session ends: nothing one
    // person copied reaches the next.
    if ctx.door("accounts logout")? != ["ok"] {
        return Err("accounts logout didn't answer ok".into());
    }
    let left = clipboard(ctx)?;
    if !left.is_empty() {
        return Err(format!("after logout the clipboard still holds {} bytes", left.len()));
    }
    Ok(())
}

/// `@settings.about.licences` (TASK-116): Settings ▸ About says the code's
/// licence (Apache-2.0), where the name and mark's terms are, and the third
/// parties' notices; the version and the slot it started from.
pub fn about_licences(ctx: &mut Ctx) -> Result<(), String> {
    signed_in(ctx)?;
    open(ctx)?;
    press_id(ctx, "settings-tab-about")?;
    chosen(ctx, "settings-tab-about")?;
    for (id, want) in [
        ("settings-about-licence", "Apache-2.0"),
        ("settings-about-brand", "TRADEMARKS.md"),
        ("settings-about-notices", "THIRD-PARTY-NOTICES.md"),
        ("settings-about-version", env!("CARGO_PKG_VERSION")),
        ("settings-about-slot", "A, build "),
    ] {
        let n = crate::sweep::until_q(ctx, &crate::session::sel(id))?;
        let v = crate::sweep::attr(ctx, n, "aria-valuenow")?.unwrap_or_default();
        if !v.contains(want) {
            return Err(format!("{id} says {v:?}, not {want:?}"));
        }
    }
    press_id(ctx, "settings-cancel")?;
    closed(ctx)
}

#[cfg(test)]
mod clock_tests {
    #[test]
    fn civil_dates_and_zone_names() {
        assert_eq!(super::civil(1_795_543_500), (2026, 11, 24, 18, 5, 0));
        assert_eq!(super::civil(951_782_400), (2000, 2, 29, 0, 0, 0));
        assert_eq!(super::civil(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(super::zone_name(-360), "UTC\u{2212}06:00");
        assert_eq!(super::zone_name(345), "UTC+05:45");
        assert_eq!(super::said_near(1_795_543_500, "UTC")[1], "18:05 UTC, 24 Nov 2026");
    }
}
