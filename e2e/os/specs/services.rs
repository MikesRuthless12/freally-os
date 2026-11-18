// SPDX-License-Identifier: Apache-2.0
//! The service specs (`@services.*`, `@log.*`), driven through conductor's
//! e2e test door on COM3 until cdpd exists (Phase 3).

use crate::{Ctx, Expect};

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

/// `@devices.pci`: devmgr's PCI list (found through ACPI's MCFG and the PCIe
/// configuration space) is exactly QEMU's own (QMP `query-pci`) (TASK-041).
pub fn pci(ctx: &mut Ctx) -> Result<(), String> {
    use fjson::Value;
    ctx.expect_log("[devmgr] devmgr: ")?;
    let ours: std::collections::BTreeSet<(u64, u64, u64, u64, u64)> = ctx
        .door("devices")?
        .iter()
        .filter_map(|l| l.strip_prefix("pci "))
        .filter_map(|l| {
            let mut parts = l.split(' ');
            let (bdf, ids) = (parts.next()?, parts.next()?);
            let (bus, rest) = bdf.split_once(':')?;
            let (dev, func) = rest.split_once('.')?;
            let (vendor, id) = ids.split_once(':')?;
            let hex = |s: &str| u64::from_str_radix(s, 16).ok();
            Some((hex(bus)?, hex(dev)?, func.parse().ok()?, hex(vendor)?, hex(id)?))
        })
        .collect();
    let q = ctx.cdp.call("Freally.qemuPci", Value::Obj(Vec::new()))?;
    let mut theirs = std::collections::BTreeSet::new();
    for bus in q.as_array().ok_or("query-pci gave no list")? {
        for d in bus.get("devices").and_then(Value::as_array).map(<[Value]>::to_vec).unwrap_or_default() {
            let n = |k: &str| d.get(k).and_then(Value::as_i64).unwrap_or(-1) as u64;
            let id = |k: &str| d.get("id").and_then(|i| i.get(k)).and_then(Value::as_i64).unwrap_or(-1) as u64;
            theirs.insert((n("bus"), n("slot"), n("function"), id("vendor"), id("device")));
        }
    }
    if theirs.is_empty() {
        return Err("QEMU reported no PCI devices".into());
    }
    if ours != theirs {
        return Err(format!("devmgr found {ours:x?}; QEMU has {theirs:x?}"));
    }
    Ok(())
}

/// `@display.fill`: drv-gop-fb maps the framebuffer write-combining (a VMO over
/// exactly the range devmgr granted); a client fills it with the UI kit's
/// ground (`--color-bg`, #090c09), and the screen is that colour, every pixel
/// (TASK-042).
pub fn fill(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("[drv-gop-fb] drv-gop-fb: ")?;
    let answer = ctx.door("fbfill 090c09")?;
    if answer.first().map(String::as_str) != Some("ok") {
        return Err(format!("fbfill: {answer:?}"));
    }
    ctx.expect_screen(Expect::Fill(0x09_0c_09))
}
