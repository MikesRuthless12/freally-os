// SPDX-License-Identifier: Apache-2.0
//! Accounts (TASK-101, PRD § 10, FR-045/046, US-015) through conductor's
//! test door, until the welcome and login screens call them (TASK-102); the
//! home each account opens is checked through the Terminal, a real caller of
//! vfs. All on the GPT disk, after the file specs (which make no accounts).

use crate::Ctx;
use crate::terminal::{expect, open};
use fjson::Value;
use std::time::{Duration, Instant};

const OWNER: &str = "mike";
const OWNER_PASSWORD: &str = "correct-horse";

fn door(ctx: &Ctx, cmd: &str) -> Result<Vec<String>, String> {
    ctx.door(&format!("accounts {cmd}"))
}

fn ok(ctx: &Ctx, cmd: &str) -> Result<(), String> {
    match door(ctx, cmd)? {
        s if s == ["ok"] => Ok(()),
        s => Err(format!("accounts {cmd}: {s:?}")),
    }
}

/// `cmd` refused, in words containing `words`.
fn refused(ctx: &Ctx, cmd: &str, words: &str) -> Result<(), String> {
    match door(ctx, cmd)?.first() {
        Some(l) if l.starts_with("error:") && l.contains(words) => Ok(()),
        other => Err(format!("accounts {cmd} should have been refused with \"{words}\"; it said {other:?}")),
    }
}

fn who(ctx: &Ctx, want: &str) -> Result<(), String> {
    match door(ctx, "who")? {
        s if s == [want] => Ok(()),
        s => Err(format!("accounts who: {s:?}, not {want}")),
    }
}

/// The Owner, made if the disk has no accounts yet (each spec can run alone).
fn owner(ctx: &Ctx) -> Result<(), String> {
    ctx.expect_log("accounts: ")?;
    if door(ctx, "list")? == ["(none)"] {
        ok(ctx, &format!("create {OWNER} {OWNER_PASSWORD}"))?;
    }
    Ok(())
}

/// Another account, made by the Owner (logged in for it, then out).
fn standard(ctx: &Ctx, user: &str, password: &str) -> Result<(), String> {
    owner(ctx)?;
    if door(ctx, "list")?.iter().any(|l| l.starts_with(&format!("{user} "))) {
        return Ok(());
    }
    ok(ctx, &format!("login {OWNER} {OWNER_PASSWORD}"))?;
    ok(ctx, &format!("create {user} {password} {OWNER_PASSWORD}"))?;
    ok(ctx, "logout")
}

/// `@accounts.lifecycle`: on a disk with none, the first account made is the
/// Owner; a bad name or a short password is refused; a wrong password says
/// so; logged in, the account's home is the session's (written and read in
/// the Terminal); logged out, it's shut; only the Owner, giving its
/// password, adds another account.
pub fn lifecycle(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("accounts: 0 account(s): the welcome screen makes the first")?;
    if door(ctx, "list")? != ["(none)"] {
        return Err("a fresh disk should list no accounts".into());
    }
    refused(ctx, &format!("create Mike {OWNER_PASSWORD}"), "A username is 2 to 32 lowercase letters")?;
    refused(ctx, &format!("create {OWNER} short"), "at least 8 characters")?;
    ok(ctx, &format!("create {OWNER} {OWNER_PASSWORD}"))?;
    ctx.expect_log(&format!("accounts: made {OWNER} (Owner)"))?;
    if door(ctx, "list")? != [format!("{OWNER} owner")] {
        return Err(format!("the first account should be the Owner: {:?}", door(ctx, "list")?));
    }
    refused(ctx, &format!("create {OWNER} another-pass"), "already an account")?;
    refused(ctx, &format!("login {OWNER} wrong-horse"), "That password didn't match.")?;
    who(ctx, "(no one)")?;
    ok(ctx, &format!("login {OWNER} {OWNER_PASSWORD}"))?;
    who(ctx, OWNER)?;
    // A password changed and back (the reset in @accounts.password ends
    // that spec's count of what accounts was asked).
    ok(ctx, &format!("passwd {OWNER_PASSWORD} interim-pass-9"))?;
    ok(ctx, &format!("passwd interim-pass-9 {OWNER_PASSWORD}"))?;
    open(ctx)?;
    expect(ctx, "whoami", &[OWNER])?;
    expect(ctx, &format!("write /home/{OWNER}/notes.txt sealed under the master key"), &[])?;
    expect(ctx, &format!("cat /home/{OWNER}/notes.txt"), &["sealed under the master key"])?;
    ok(ctx, "logout")?;
    who(ctx, "(no one)")?;
    expect(ctx, "whoami", &["No one"])?;
    expect(ctx, &format!("cat /home/{OWNER}/notes.txt"), &["AccessDenied"])?;
    // A second account: the Owner's to add, with the Owner's password.
    refused(ctx, "create dana dana-pass-1", "Only the Owner adds accounts.")?;
    ok(ctx, &format!("login {OWNER} {OWNER_PASSWORD}"))?;
    refused(ctx, "create dana dana-pass-1 wrong-horse", "That password didn't match.")?;
    ok(ctx, &format!("create dana dana-pass-1 {OWNER_PASSWORD}"))?;
    ok(ctx, "logout")?;
    let list = door(ctx, "list")?;
    if !list.iter().any(|l| l == "dana standard") || !list.contains(&format!("{OWNER} owner")) {
        return Err(format!("accounts list: {list:?}"));
    }
    // Dana's home is Dana's: the Owner's file isn't in it.
    ok(ctx, "login dana dana-pass-1")?;
    expect(ctx, "whoami", &["dana"])?;
    expect(ctx, "ls /home", &["dana/"])?;
    expect(ctx, &format!("cat /home/{OWNER}/notes.txt"), &["AccessDenied"])?;
    ok(ctx, "logout")
}

/// `@accounts.throttle` (US-015): the first two wrong passwords answer at
/// once; from the third, each makes the next try wait a second more, and a
/// try sooner is turned away unheard, even with the right password.
pub fn throttle(ctx: &mut Ctx) -> Result<(), String> {
    owner(ctx)?;
    let wrong = format!("login {OWNER} not-the-password");
    let right = format!("login {OWNER} {OWNER_PASSWORD}");
    for _ in 0..3 {
        refused(ctx, &wrong, "That password didn't match.")?;
    }
    // The third: a second before the next is heard.
    refused(ctx, &wrong, "Wait a moment")?;
    std::thread::sleep(Duration::from_millis(1100));
    refused(ctx, &wrong, "That password didn't match.")?;
    // The fourth: two seconds, the right password included.
    refused(ctx, &right, "Wait a moment")?;
    std::thread::sleep(Duration::from_millis(1100));
    refused(ctx, &right, "Wait a moment")?;
    std::thread::sleep(Duration::from_millis(1100));
    ok(ctx, &right)?;
    ctx.expect_log("accounts: a wrong password for mike (4 in a row)")?;
    // A success starts the count again.
    ok(ctx, "logout")?;
    refused(ctx, &wrong, "That password didn't match.")?;
    ok(ctx, &right)?;
    ok(ctx, "logout")
}

/// `@accounts.password`: a password change needs the old one and a new one
/// of 8 or more; after it the old one is refused and the new one opens the
/// same home, its files untouched, and still does after the machine resets.
pub fn password(ctx: &mut Ctx) -> Result<(), String> {
    standard(ctx, "pat", "pat-first-1")?;
    refused(ctx, "passwd pat-first-1 pat-second-2", "Log in first.")?;
    ok(ctx, "login pat pat-first-1")?;
    open(ctx)?;
    expect(ctx, "write /home/pat/kept.txt across a password change", &[])?;
    refused(ctx, "passwd not-pats pat-second-2", "That password didn't match.")?;
    refused(ctx, "passwd pat-first-1 short", "at least 8 characters")?;
    ok(ctx, "passwd pat-first-1 pat-second-2")?;
    ctx.expect_log("accounts: pat's password changed")?;
    ok(ctx, "logout")?;
    refused(ctx, "login pat pat-first-1", "That password didn't match.")?;
    ok(ctx, "login pat pat-second-2")?;
    expect(ctx, "cat /home/pat/kept.txt", &["across a password change"])?;
    ok(ctx, "logout")?;
    // The record on disk is the new one.
    let counted = ctx.log_count("account(s)");
    ctx.reset()?;
    let start = Instant::now();
    while ctx.log_count("account(s)") <= counted {
        if start.elapsed() > Duration::from_secs(60) {
            return Err("after the reset, accounts never said how many accounts it found".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    refused(ctx, "login pat pat-first-1", "That password didn't match.")?;
    ok(ctx, "login pat pat-second-2")?;
    open(ctx)?;
    expect(ctx, "cat /home/pat/kept.txt", &["across a password change"])?;
    ok(ctx, "logout")
}

/// The e2e image's master key for `user` (accounts' `master_key`).
fn e2e_key(user: &str) -> String {
    fcrypto::sha2::sha256(format!("freally e2e master key: {user}").as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// How many places the machine's RAM holds `hex`, and the first few.
fn scan(ctx: &mut Ctx, hex: &str) -> Result<(usize, String), String> {
    let r = ctx.cdp.call("Freally.scanMemory", Value::obj([("pattern", hex.into())]))?;
    let found = r.get("found").and_then(Value::as_i64).ok_or("no count")? as usize;
    Ok((found, r.get("at").map(Value::to_json).unwrap_or_default()))
}

/// `@accounts.key_gone` (FR-046): an account's master key is in the
/// machine's RAM only while it's logged in. Logged in, a scan of all of
/// guest RAM finds it (ffs holds it, so the scan can see a key); logged
/// out, nowhere: not in accounts, ffs, the kernel's messages, or a stack.
pub fn key_gone(ctx: &mut Ctx) -> Result<(), String> {
    standard(ctx, "keyscan", "keyscan-pass")?;
    let key = e2e_key("keyscan");
    ok(ctx, "login keyscan keyscan-pass")?;
    let (held, _) = scan(ctx, &key)?;
    if held == 0 {
        return Err("logged in, the scan didn't find the master key: it can't be trusted to say it's gone".into());
    }
    ok(ctx, "logout")?;
    let (left, at) = scan(ctx, &key)?;
    if left != 0 {
        return Err(format!("after logout the master key is still in RAM in {left} place(s): {at}"));
    }
    Ok(())
}

/// `@accounts.export` (TASK-103, US-016): the session's home, with the
/// password typed again, sealed into `Exports/<user>.facct` (the Exports
/// folder itself left out); a wrong password refused; the bundle listed,
/// and its preview opens only with the password.
pub fn export(ctx: &mut Ctx) -> Result<(), String> {
    owner(ctx)?;
    ok(ctx, &format!("login {OWNER} {OWNER_PASSWORD}"))?;
    open(ctx)?;
    expect(ctx, &format!("mkdir /home/{OWNER}/Documents"), &[])?;
    expect(ctx, &format!("write /home/{OWNER}/Documents/plan.txt the plan, carried"), &[])?;
    refused(ctx, "export not-the-password", "That password didn't match.")?;
    let said = door(ctx, &format!("export {OWNER_PASSWORD}"))?;
    let want = format!("/home/{OWNER}/Exports/{OWNER}.facct ");
    if said.len() != 1 || !said[0].starts_with(&want) {
        return Err(format!("accounts export: {said:?}"));
    }
    ctx.expect_log(&format!("accounts: exported {OWNER} to /home/{OWNER}/Exports/{OWNER}.facct"))?;
    expect(ctx, &format!("ls /home/{OWNER}/Exports"), &[&format!("{OWNER}.facct")])?;
    if !door(ctx, "bundles")?.contains(&format!("home:{OWNER}.facct")) {
        return Err(format!("accounts bundles: {:?}", door(ctx, "bundles")?));
    }
    refused(ctx, &format!("preview home:{OWNER}.facct wrong-password"), "That password doesn't open this file.")?;
    let p = door(ctx, &format!("preview home:{OWNER}.facct {OWNER_PASSWORD}"))?;
    if p.len() != 1 || !p[0].starts_with(&format!("{OWNER} files=")) {
        return Err(format!("accounts preview: {p:?}"));
    }
    ok(ctx, "logout")
}

/// `@accounts.import` (TASK-103, US-016, FR-047): a bundle made into an
/// account: the same username refused (and named), a wrong password
/// refused, renamed by the Owner it's made; its files are there under the
/// bundle's password, without the Exports folder.
pub fn import(ctx: &mut Ctx) -> Result<(), String> {
    owner(ctx)?;
    ok(ctx, &format!("login {OWNER} {OWNER_PASSWORD}"))?;
    if !door(ctx, "bundles")?.contains(&format!("home:{OWNER}.facct")) {
        door(ctx, &format!("export {OWNER_PASSWORD}"))?;
    }
    let name = format!("home:{OWNER}.facct");
    refused(ctx, &format!("import {name} {OWNER_PASSWORD}"), &format!("There's already an account called {OWNER}."))?;
    refused(
        ctx,
        &format!("import {name} wrong-password carried {OWNER_PASSWORD}"),
        "That password doesn't open this file.",
    )?;
    let made = door(ctx, &format!("import {name} {OWNER_PASSWORD} carried {OWNER_PASSWORD}"))?;
    if made != ["carried standard"] {
        return Err(format!("accounts import: {made:?}"));
    }
    ctx.expect_log(&format!("accounts: imported carried (Standard) from {name}"))?;
    ok(ctx, "logout")?;
    ok(ctx, &format!("login carried {OWNER_PASSWORD}"))?;
    open(ctx)?;
    expect(ctx, "whoami", &["carried"])?;
    expect(ctx, "cat /home/carried/Documents/plan.txt", &["the plan, carried"])?;
    let listed = expect(ctx, "ls /home/carried", &["Documents/"])?;
    if listed.contains("Exports/") {
        return Err(format!("the Exports folder was carried too:\n{listed}"));
    }
    ok(ctx, "logout")?;
    // Login, with every account this disk now has: each row chosen by a click.
    const ROW: &str = "WINDOW[data-owner=\"login\"] OPTION";
    crate::sweep::until_q(ctx, ROW)?;
    for id in crate::sweep::q_all(ctx, ROW)? {
        let testid = crate::sweep::attr(ctx, id, "data-testid")?.ok_or("a login row without a test id")?;
        crate::sweep::click(ctx, id)?;
        crate::sweep::until_attr(ctx, &format!("[data-testid=\"{testid}\"]"), "aria-selected", "true")?;
    }
    Ok(())
}
