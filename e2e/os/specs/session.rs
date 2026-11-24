// SPDX-License-Identifier: Apache-2.0
//! Welcome and Login (TASK-102, PRD § 8, FR-045), on a GPT disk of their
//! own: every field, error and key worked with the real keyboard and mouse,
//! the drawing checked in both themes; Login → desktop timed
//! (`@accounts.login`, ≤ 1 s); Restart and Shut down through conductor, ffs
//! unmounting cleanly first.

use crate::Ctx;
use crate::stage::key;
use crate::sweep::{attr, box_of, click, until_attr, until_gone, until_q};
use fui::tokens_gen::{dark, light};
use std::time::{Duration, Instant};

const OWNER: &str = "mike";
const PASSWORD: &str = "correct-horse";

pub(crate) fn sel(id: &str) -> String {
    format!("[data-testid=\"{id}\"]")
}

pub(crate) fn press(ctx: &mut Ctx, code: &str) -> Result<(), String> {
    key(ctx, code, true)?;
    key(ctx, code, false)
}

fn shifted(ctx: &mut Ctx, code: &str) -> Result<(), String> {
    key(ctx, "ShiftLeft", true)?;
    press(ctx, code)?;
    key(ctx, "ShiftLeft", false)
}

/// Text typed on the real keyboard: letters, digits, space, - and _.
pub(crate) fn typed(ctx: &mut Ctx, text: &str) -> Result<(), String> {
    for c in text.chars() {
        match c {
            'a'..='z' => press(ctx, &format!("Key{}", c.to_ascii_uppercase()))?,
            'A'..='Z' => shifted(ctx, &format!("Key{c}"))?,
            '0'..='9' => press(ctx, &format!("Digit{c}"))?,
            ' ' => press(ctx, "Space")?,
            '-' => press(ctx, "Minus")?,
            '_' => shifted(ctx, "Minus")?,
            _ => return Err(format!("typed() can't type {c:?}")),
        }
    }
    Ok(())
}

/// What a field holds, as the tree says (dots for a password).
pub(crate) fn value(ctx: &mut Ctx, id: &str) -> Result<String, String> {
    let n = until_q(ctx, &sel(id))?;
    Ok(attr(ctx, n, "aria-valuenow")?.unwrap_or_default())
}

pub(crate) fn expect_value(ctx: &mut Ctx, id: &str, want: &str) -> Result<(), String> {
    let start = Instant::now();
    loop {
        let v = value(ctx, id)?;
        if v == want {
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err(format!("{id} holds {v:?}, not {want:?}"));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

pub(crate) fn expect_focus(ctx: &mut Ctx, id: &str) -> Result<(), String> {
    until_attr(ctx, &sel(id), "data-focused", "true")
}

/// The error under field `id` says `want`.
pub(crate) fn expect_error(ctx: &mut Ctx, id: &str, want: &str) -> Result<(), String> {
    until_attr(ctx, &sel(&format!("{id}-error")), "aria-label", want)
}

/// The theme (through stage, as Settings will), and the inside of the well
/// `id` drawn in its ground.
fn both_themes(ctx: &mut Ctx, id: &str) -> Result<(), String> {
    for (theme, bg) in [("light", light::COLOR_BG.xrgb()), ("dark", dark::COLOR_BG.xrgb())] {
        let said = ctx.door(&format!("stage theme {theme}"))?;
        if said != ["ok"] {
            return Err(format!("stage theme {theme}: {said:?}"));
        }
        let n = until_q(ctx, &sel(id))?;
        let (_, y0, x1, y1) = box_of(ctx, n)?;
        let start = Instant::now();
        loop {
            // Inside the well, past its ring and before any dot.
            let p = crate::stage::pixel(ctx, (x1 - 6) as i32, ((y0 + y1) / 2) as i32)? & 0xFF_FFFF;
            if p == bg {
                break;
            }
            if start.elapsed() > Duration::from_secs(10) {
                return Err(format!("in the {theme} theme {id}'s well is {p:06x}, not {bg:06x}"));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        ctx.keep_shot(&format!("{id}-{theme}"))?;
    }
    Ok(())
}

pub(crate) fn waits_for_log(ctx: &Ctx, text: &str, more_than: usize) -> Result<Duration, String> {
    let start = Instant::now();
    while ctx.log_count(text) <= more_than {
        if start.elapsed() > Duration::from_secs(20) {
            return Err(format!("COM1 never said {text:?}"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(start.elapsed())
}

/// `@welcome.form`: on a disk with no accounts, Welcome is up (the desktop
/// locked); Enter walks the fields; every mistake is said under its field
/// (no name, a bad username, a short password, passwords that differ); the
/// name fills the username; a click focuses a field; both themes; *Create
/// account* makes the Owner and logs it in, and the desktop is theirs.
pub fn welcome_form(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("welcome: shown")?;
    ctx.expect_log("desktop: locked: welcome is up")?;
    // The licence needs nothing accepted (TASK-116).
    expect_value(
        ctx,
        "welcome-licence",
        "Freally OS is open source under the Apache License 2.0: there's nothing to accept. Settings ▸ About has the licences.",
    )?;
    expect_focus(ctx, "welcome-name")?;
    for id in ["welcome-username", "welcome-password", "welcome-again"] {
        press(ctx, "Enter")?;
        expect_focus(ctx, id)?;
    }
    press(ctx, "Enter")?;
    expect_error(ctx, "welcome-name", "Type your name.")?;
    expect_error(ctx, "welcome-username", "Usernames start with a letter and use a–z, 0–9, - or _.")?;
    expect_error(ctx, "welcome-password", "A password needs at least 8 characters.")?;
    expect_focus(ctx, "welcome-name")?;
    typed(ctx, "Mike Weaver")?;
    expect_value(ctx, "welcome-name", "Mike Weaver")?;
    expect_value(ctx, "welcome-username", "mike-weaver")?;
    until_gone(ctx, &sel("welcome-name-error"))?;
    // A click focuses a field; dots, never the password, in the tree.
    let pw = until_q(ctx, &sel("welcome-password"))?;
    click(ctx, pw)?;
    expect_focus(ctx, "welcome-password")?;
    typed(ctx, PASSWORD)?;
    expect_value(ctx, "welcome-password", &"•".repeat(PASSWORD.len()))?;
    press(ctx, "Tab")?;
    expect_focus(ctx, "welcome-again")?;
    typed(ctx, "correct-hors")?;
    press(ctx, "Enter")?;
    expect_error(ctx, "welcome-again", "The passwords don't match.")?;
    typed(ctx, "e")?;
    until_gone(ctx, &sel("welcome-again-error"))?;
    // Shift+Tab goes back; the username typed into stays as typed.
    shifted(ctx, "Tab")?;
    shifted(ctx, "Tab")?;
    expect_focus(ctx, "welcome-username")?;
    for _ in 0.."mike-weaver".len() {
        press(ctx, "Backspace")?;
    }
    typed(ctx, "9x")?;
    let create = until_q(ctx, &sel("welcome-create"))?;
    click(ctx, create)?;
    expect_error(ctx, "welcome-username", "Usernames start with a letter and use a–z, 0–9, - or _.")?;
    press(ctx, "Backspace")?;
    press(ctx, "Backspace")?;
    typed(ctx, OWNER)?;
    expect_value(ctx, "welcome-username", OWNER)?;
    both_themes(ctx, "welcome-name")?;
    let made = ctx.log_count("desktop: mike's session");
    let create = until_q(ctx, &sel("welcome-create"))?;
    click(ctx, create)?;
    waits_for_log(ctx, "welcome: made mike (the Owner) and logged in", 0)?;
    waits_for_log(ctx, "desktop: mike's session", made)?;
    if ctx.door("accounts list")? != [format!("{OWNER} owner")] {
        return Err(format!("accounts list: {:?}", ctx.door("accounts list")?));
    }
    until_gone(ctx, &sel("welcome-name"))?;
    Ok(())
}

/// Logs out (the door, until Settings has the key) and waits for Login.
pub(crate) fn to_login(ctx: &mut Ctx) -> Result<(), String> {
    // Already there when an earlier spec logged out last; either way the
    // screen itself is waited for, not a line (it may have been said already).
    if ctx.door("accounts who")? != ["(no one)"] && ctx.door("accounts logout")? != ["ok"] {
        return Err("accounts logout didn't".into());
    }
    until_q(ctx, &sel("login-password"))?;
    expect_focus(ctx, "login-password")
}

/// `@login.form`: logged out, Login is up over the dimmed wallpaper; a wrong
/// password says so and empties the well; ↑ ↓ and a click choose an
/// account; Tab reaches Shut down and Restart and comes back; both themes;
/// *Log in* opens the desktop.
pub fn login_form(ctx: &mut Ctx) -> Result<(), String> {
    to_login(ctx)?;
    until_attr(ctx, &sel("login-user-mike"), "aria-selected", "true")?;
    typed(ctx, "wrong-pass")?;
    expect_value(ctx, "login-password", &"•".repeat(10))?;
    press(ctx, "Enter")?;
    expect_error(ctx, "login-password", "That password didn't match.")?;
    expect_value(ctx, "login-password", "")?;
    // A second account (made by the Owner): both listed, each chosen in turn.
    for cmd in [format!("login {OWNER} {PASSWORD}"), format!("create dana dana-pass-1 {PASSWORD}")] {
        let said = ctx.door(&format!("accounts {cmd}"))?;
        if said != ["ok"] {
            return Err(format!("accounts {cmd}: {said:?}"));
        }
    }
    to_login(ctx)?;
    until_q(ctx, &sel("login-user-dana"))?;
    press(ctx, "ArrowDown")?;
    until_attr(ctx, &sel("login-user-dana"), "aria-selected", "true")?;
    press(ctx, "ArrowUp")?;
    until_attr(ctx, &sel("login-user-mike"), "aria-selected", "true")?;
    let dana = until_q(ctx, &sel("login-user-dana"))?;
    click(ctx, dana)?;
    until_attr(ctx, &sel("login-user-dana"), "aria-selected", "true")?;
    let mike = until_q(ctx, &sel("login-user-mike"))?;
    click(ctx, mike)?;
    until_attr(ctx, &sel("login-user-mike"), "aria-selected", "true")?;
    for id in ["login-shutdown", "login-restart", "login-password"] {
        press(ctx, "Tab")?;
        expect_focus(ctx, id)?;
    }
    both_themes(ctx, "login-password")?;
    typed(ctx, PASSWORD)?;
    let made = ctx.log_count("desktop: mike's session");
    let submit = until_q(ctx, &sel("login-submit"))?;
    click(ctx, submit)?;
    waits_for_log(ctx, "desktop: mike's session", made)?;
    until_gone(ctx, &sel("login-password"))?;
    Ok(())
}

/// `@accounts.login` (PRD § 7: Login → desktop ≤ 1 s, Argon2id included):
/// from Enter in the password well to the desktop saying whose it is.
pub fn login_time(ctx: &mut Ctx) -> Result<(), String> {
    to_login(ctx)?;
    until_attr(ctx, &sel("login-user-mike"), "aria-selected", "true")?;
    typed(ctx, PASSWORD)?;
    let made = ctx.log_count("desktop: mike's session");
    key(ctx, "Enter", true)?;
    let took = waits_for_log(ctx, "desktop: mike's session", made)?;
    key(ctx, "Enter", false)?;
    let ms = took.as_millis();
    ctx.note(&format!("Login → desktop: {ms} ms (budget 1,000)"));
    if ms > 1000 {
        return Err(format!("Login → desktop took {ms} ms (over 1,000)"));
    }
    Ok(())
}

/// `@login.restart`: *Restart* (Tab to it, Enter): ffs unmounts cleanly,
/// the machine restarts, mounts without ffsck, and Login is up again.
pub fn login_restart(ctx: &mut Ctx) -> Result<(), String> {
    to_login(ctx)?;
    press(ctx, "Tab")?;
    press(ctx, "Tab")?;
    expect_focus(ctx, "login-restart")?;
    let (boots, shown) = (ctx.log_count("freally-boot "), ctx.log_count("login: shown"));
    let unmounted = ctx.log_count("ffs: unmounted cleanly");
    ctx.rebooted_by_guest();
    press(ctx, "Enter")?;
    waits_for_log(ctx, "conductor: restarting (login asked)", 0)?;
    waits_for_log(ctx, "ffs: unmounted cleanly", unmounted)?;
    waits_for_log(ctx, "freally-boot ", boots)?;
    waits_for_log(ctx, "login: shown", shown)?;
    let tail = ctx.log();
    let since = tail.rfind("freally-boot ").map_or("", |i| &tail[i..]);
    if since.contains("wasn't unmounted") || !since.contains("ffs: mounted generation") {
        return Err("after Restart the volume wasn't mounted clean".into());
    }
    Ok(())
}

/// `@login.shutdown` (its own disk): *Shut down* by the mouse: ffs
/// unmounts cleanly, and the machine powers off (QEMU exits).
pub fn login_shutdown(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("welcome: shown")?;
    let shown = ctx.log_count("login: shown");
    if ctx.door(&format!("accounts create {OWNER} {PASSWORD}"))? != ["ok"] {
        return Err("accounts create didn't".into());
    }
    waits_for_log(ctx, "login: shown", shown)?;
    let off = until_q(ctx, &sel("login-shutdown"))?;
    ctx.rebooted_by_guest();
    click(ctx, off)?;
    waits_for_log(ctx, "conductor: shutting down (login asked)", 0)?;
    waits_for_log(ctx, "ffs: unmounted cleanly", 0)?;
    let start = Instant::now();
    while ctx.cdp.call("Freally.qemuPci", fjson::Value::Obj(Vec::new())).is_ok() {
        if start.elapsed() > Duration::from_secs(20) {
            return Err("Shut down: the machine is still on 20 s later".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(())
}
