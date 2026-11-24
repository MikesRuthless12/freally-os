// SPDX-License-Identifier: Apache-2.0
//! Offline updates (TASK-110, FR-050), through Settings ▸ Update on a GPT
//! disk whose `/system/updates` holds (put there by xtask before it boots):
//! the next version, a copy of it with one byte of the image changed, its
//! header signed by no trusted key, and a version whose system never starts.
//! In order, in one VM: the bad ones refused (`@update.bad-signature`), the
//! next version installed and started (`@update.install`), switched back
//! (`@update.switch-back`), and the broken one falling back by itself
//! (`@update.fallback`).

use crate::Ctx;
use crate::session::{expect_value, sel};
use crate::settings::{chosen, open, press_id, signed_in};
use crate::sweep::{attr, until_q};
use std::time::{Duration, Instant};

/// The running version, and the ones the packages bring.
fn versions() -> (String, String, String) {
    let v = env!("CARGO_PKG_VERSION");
    let mut p: Vec<u64> = v.split('.').map(|x| x.parse().unwrap_or(0)).collect();
    p.resize(3, 0);
    let at = |by: u64| format!("{}.{}.{}", p[0], p[1], p[2] + by);
    (at(0), at(1), at(2))
}

/// How many times ffs has mounted (or made) the disk, this VM's boots together.
fn mounts(ctx: &Ctx) -> usize {
    ctx.log_count("ffs: mounted generation") + ctx.log_count("ffs: formatted")
}

/// Waits (2 minutes) for ffs's mount number `n`: the boot's disk is up, so
/// accounts can be.
fn mounted(ctx: &Ctx, n: usize) -> Result<(), String> {
    let start = Instant::now();
    while mounts(ctx) < n {
        if start.elapsed() > Duration::from_secs(120) {
            return Err("ffs never mounted the disk".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(())
}

/// Settings open on Update (once ffs has mounted the disk the packages are on).
fn update_panel(ctx: &mut Ctx) -> Result<(), String> {
    mounted(ctx, 1)?;
    signed_in(ctx)?;
    open(ctx)?;
    press_id(ctx, "settings-tab-update")?;
    chosen(ctx, "settings-tab-update")?;
    until_q(ctx, &sel("settings-update-line-0"))?;
    Ok(())
}

/// Waits (`secs`) for test id `id` to hold `want`.
fn waits_for(ctx: &mut Ctx, id: &str, want: &str, secs: u64) -> Result<(), String> {
    let start = Instant::now();
    loop {
        let now = match until_q(ctx, &sel(id)) {
            Ok(n) => attr(ctx, n, "aria-valuenow")?.unwrap_or_default(),
            Err(_) => String::new(),
        };
        if now == want {
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(secs) {
            return Err(format!("{id} holds {now:?}, not {want:?}"));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// The package `name` chosen: what Settings says of it.
fn choose(ctx: &mut Ctx, name: &str) -> Result<String, String> {
    press_id(ctx, &format!("settings-update-file-{name}"))?;
    let n = until_q(ctx, &sel("settings-update-chosen"))?;
    Ok(attr(ctx, n, "aria-valuenow")?.unwrap_or_default())
}

/// The A/B record has `line` (`slot B Trial 3 0.4.1`, `active B` …).
fn record_has(ctx: &mut Ctx, line: &str) -> Result<(), String> {
    let state = ctx.door("bootctl")?;
    if state.iter().any(|l| l.starts_with(line)) {
        Ok(())
    } else {
        Err(format!("the A/B record is {state:?}, wanted {line:?}"))
    }
}

/// Restart now, from the Update panel: the machine restarts itself; waits
/// for `then` on COM1 (its next count).
fn restart_until(ctx: &mut Ctx, then: &str) -> Result<(), String> {
    let n = ctx.log_count(then);
    press_id(ctx, "settings-update-restart")?;
    // A whole restart: the services stopped, ffs unmounted, the firmware,
    // and a 200 MB image read and verified. A minute, not the usual 10 s.
    let start = Instant::now();
    while ctx.log_count(then) <= n {
        if start.elapsed() > Duration::from_secs(60) {
            return Err(format!("no {then:?} within a minute of Restart now"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

/// `@update.bad-signature`: a package signed by no trusted key is refused
/// when it's chosen; one whose image was changed is refused when it's
/// installed, and leaves the other slot as it was; one cut short is given
/// up when it ends (Settings cancels the updater's install), the slot Empty.
pub fn bad_signature(ctx: &mut Ctx) -> Result<(), String> {
    let (_, next, _) = versions();
    update_panel(ctx)?;
    let said = choose(ctx, "foreign.fup")?;
    if said != "This update wasn't signed by a key this computer trusts." {
        return Err(format!("foreign.fup: {said:?}"));
    }
    if crate::sweep::q(ctx, &sel("settings-update-install"))? != 0 {
        return Err("an Install key for a package nobody trusts".into());
    }
    let said = choose(ctx, "changed.fup")?;
    if !said.starts_with(&format!("Freally OS {next} · signed with the dev key")) {
        return Err(format!("changed.fup, before it's read whole: {said:?}"));
    }
    press_id(ctx, "settings-update-install")?;
    waits_for(ctx, "settings-update-note", "This update was changed after it was signed.", 300)?;
    record_has(ctx, "slot B Empty")?;
    record_has(ctx, "active A")?;
    if ctx.log_count("written to slot B and checked") != 0 {
        return Err("the changed image was taken".into());
    }
    let said = choose(ctx, "short.fup")?;
    if !said.starts_with(&format!("Freally OS {next} · signed with the dev key")) {
        return Err(format!("short.fup, its header whole: {said:?}"));
    }
    press_id(ctx, "settings-update-install")?;
    waits_for(ctx, "settings-update-note", "The update file ended early.", 300)?;
    record_has(ctx, "slot B Empty")?;
    record_has(ctx, "active A")?;
    Ok(())
}

/// `@update.install`: the next version chosen (its version, key, size and
/// notes), installed with the meter, on trial from the restart, started,
/// and marked Good.
pub fn install(ctx: &mut Ctx) -> Result<(), String> {
    let (_, next, _) = versions();
    update_panel(ctx)?;
    let said = choose(ctx, &format!("e2e-{next}.fup"))?;
    if !said.starts_with(&format!("Freally OS {next} · signed with the dev key · ")) {
        return Err(format!("e2e-{next}.fup: {said:?}"));
    }
    press_id(ctx, "settings-update-install")?;
    waits_for(
        ctx,
        "settings-update-note",
        &format!("Freally OS {next} is installed: it starts when you restart."),
        300,
    )?;
    ctx.expect_log(&format!("updater: {next} written to slot B and checked"))?;
    expect_value(ctx, "settings-update-line-1", &format!("{next} is installed: it starts when you restart."))?;
    record_has(ctx, &format!("slot B Trial 3 {next}"))?;
    record_has(ctx, "active B")?;
    let before = mounts(ctx);
    restart_until(ctx, "freally-boot: booting slot B (trial, 2 tries left)")?;
    ctx.expect_log(&format!("conductor: started from slot B (Trial, {next})"))?;
    mounted(ctx, before + 1)?;
    signed_in(ctx)?;
    let start = Instant::now();
    while ctx.log_count(&format!("slot B ({next}) started cleanly: marked Good")) == 0 {
        if start.elapsed() > Duration::from_secs(60) {
            return Err("conductor never marked the update Good".into());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    record_has(ctx, "slot B Good")?;
    record_has(ctx, "booted B")
}

/// `@update.switch-back`: on the update, Switch back to the version before
/// it, restart, and slot A runs again.
pub fn switch_back(ctx: &mut Ctx) -> Result<(), String> {
    let (now, _, _) = versions();
    update_panel(ctx)?;
    expect_value(
        ctx,
        "settings-update-line-1",
        &format!("Freally OS {now} is on this computer too: you can switch back to it."),
    )?;
    press_id(ctx, "settings-update-switch-back")?;
    waits_for(ctx, "settings-update-note", "Done: restart to switch back.", 20)?;
    record_has(ctx, "active A")?;
    restart_until(ctx, &format!("conductor: started from slot A (Good, {now})"))?;
    record_has(ctx, "booted A")
}

/// `@update.fallback`: a version whose system never starts, installed: its
/// three trial boots crash, the fourth lands on the version before, and
/// the log says the update didn't start.
pub fn fallback(ctx: &mut Ctx) -> Result<(), String> {
    let (now, _, broken) = versions();
    ctx.allow_errors(&["freally-core: ERROR", "showing the panic screen", "can't start", "didn't start"]);
    update_panel(ctx)?;
    choose(ctx, &format!("e2e-{broken}-crash.fup"))?;
    press_id(ctx, "settings-update-install")?;
    waits_for(
        ctx,
        "settings-update-note",
        &format!("Freally OS {broken} is installed: it starts when you restart."),
        300,
    )?;
    let crashes = ctx.log_count("freally-core: ERROR");
    restart_until(ctx, "freally-boot: booting slot B (trial, 2 tries left)")?;
    crate::boot::reset_wait_crash(ctx, crashes + 1)?;
    for left in [1, 0] {
        let crashes = ctx.log_count("freally-core: ERROR");
        crate::boot::reset_until(ctx, &format!("freally-boot: booting slot B (trial, {left} tries left)"), 1)?;
        crate::boot::reset_wait_crash(ctx, crashes + 1)?;
    }
    let said = format!("The update to {broken} didn't start, so Freally OS went back to {now}.");
    crate::boot::reset_until(ctx, &said, 1)?;
    record_has(ctx, &format!("slot B Bad 0 {broken}"))?;
    record_has(ctx, "booted A")
}
