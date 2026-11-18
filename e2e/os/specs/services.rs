// SPDX-License-Identifier: Apache-2.0
//! The service specs (`@services.*`, `@log.*`), driven through conductor's
//! e2e test door on COM3 until cdpd exists (Phase 3).

use crate::Ctx;

/// `@services.restart`: every service that restarts is killed in turn, and
/// conductor has it back (started and registered again) within 200 ms
/// (TASK-038).
pub fn restart(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("[logger] logger: ready")?;
    let listed = ctx.door("services")?;
    let restartable: Vec<String> = listed
        .iter()
        .filter_map(|l| l.strip_prefix("svc "))
        .filter_map(|l| l.split_once(' '))
        .filter(|(_, state)| *state == "Running")
        .map(|(n, _)| n.to_string())
        .collect();
    if restartable.is_empty() {
        return Err(format!("no running service to kill: {listed:?}"));
    }
    for name in &restartable {
        let answer = ctx.door(&format!("kill {name}"))?;
        if answer.first().map(String::as_str) != Some("ok") {
            return Err(format!("kill {name}: {answer:?}"));
        }
        let back = answer
            .iter()
            .find_map(|l| l.strip_prefix(&format!("back {name} ")))
            .ok_or(format!("{name} never came back: {answer:?}"))?;
        let ms: u64 = back.parse().map_err(|_| format!("an unreadable time: {back:?}"))?;
        if ms > 200 {
            return Err(format!("{name} took {ms} ms to come back (at most 200)"));
        }
    }
    Ok(())
}

/// `@log.read`: the logger keeps every service's lines and the kernel's own
/// (drained from its early ring), mirrors services' lines to COM1, and `read`
/// pages them back (TASK-039).
pub fn read(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("[logger] logger: ready (mirroring to COM1)")?;
    ctx.expect_log("[conductor] conductor: logger ready")?;
    let recs = ctx.door("log 200")?;
    for want in [
        "rec kernel freally-core: conductor started (C0 ",
        "rec logger logger: ready (mirroring to COM1)",
        "rec conductor conductor: logger ready",
    ] {
        if !recs.iter().any(|r| r.starts_with(want)) {
            return Err(format!("the log has no {want:?} (it has {} records)", recs.len()));
        }
    }
    Ok(())
}
