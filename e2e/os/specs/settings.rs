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
        ("ArrowDown", "settings-tab-update"),
        ("ArrowDown", "settings-tab-hardware"),
        ("ArrowDown", "settings-tab-about"),
        ("ArrowDown", "settings-tab-personalisation"),
        ("ArrowUp", "settings-tab-about"),
        ("Home", "settings-tab-personalisation"),
        ("End", "settings-tab-about"),
        ("Home", "settings-tab-personalisation"),
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
