// SPDX-License-Identifier: Apache-2.0
//! The CDP methods cdp-host serves: from QMP, or (e2e images) from cdpd
//! through the test door (TASK-063). Without a door, cdpd's methods say so.

use crate::door::Door;
use cdp_proto::qmp::Qmp;
use fjson::Value;
use std::path::Path;

/// The methods cdpd serves.
fn cdpd_serves(m: &str) -> bool {
    m.starts_with("DOM.")
        || m.starts_with("Accessibility.")
        || m == "Log.read"
        || m == "Performance.getMetrics"
        || m == "Target.getTargets"
        || matches!(
            m,
            "Freally.getBootTimeline"
                | "Freally.getProcesses"
                | "Freally.getFaults"
                | "Freally.getManifest"
                | "Freally.readBack"
                | "Freally.inventory"
                | "Freally.nodeAt"
                | "Freally.focusedNode"
                | "Freally.ordinals"
        )
}

/// Serves one CDP call. `Ok(result)` or `Err((code, message))`.
pub fn call(
    qmp: &mut Qmp,
    door: Option<&Door>,
    shot_dir: &Path,
    method: &str,
    params: &Value,
) -> Result<Value, (i64, String)> {
    if cdpd_serves(method)
        && let Some(d) = door
    {
        return d.cdpd(method, params).map_err(|e| (-32000, e));
    }
    if method == "Target.activateTarget"
        && let Some(d) = door
    {
        let id = params.get("targetId").and_then(Value::as_str).unwrap_or("");
        let lines = d.line(&format!("stage focus {id}")).map_err(|e| (-32000, e))?;
        return match lines.iter().find(|l| l.starts_with("error")) {
            Some(e) => Err((-32000, e.clone())),
            None => Ok(Value::Obj(Vec::new())),
        };
    }
    match method {
        "Page.captureScreenshot" => screenshot(qmp, shot_dir, params).map_err(|e| (-32000, e)),
        "Input.dispatchKeyEvent" => key(qmp, params).map_err(|e| (-32000, e)),
        "Input.dispatchMouseEvent" => mouse(qmp, params).map_err(|e| (-32000, e)),
        // A warm reboot: QMP `system_reset` keeps RAM (the last-panic page survives it).
        "Page.reload" => qmp.execute("system_reset", None).map(|_| Value::Obj(Vec::new())).map_err(|e| (-32000, e)),
        "Target.getTargets" => Ok(Value::obj([("targetInfos", Value::Arr(vec![desktop_target()]))])),
        // The harness's view of QEMU's PCI bus (QMP query-pci), to check devmgr's.
        "Freally.qemuPci" => qmp.execute("query-pci", None).map_err(|e| (-32000, e)),
        // The NVMe disk slowed (QMP block_set_io_throttle on `disk0`): writes
        // at `writeBps` bytes a second, 0 for none (TASK-185, @nvme.slow).
        "Freally.qemuThrottle" => {
            let bps = params.get("writeBps").and_then(Value::as_f64).unwrap_or(0.0).max(0.0);
            let n = |v: f64| Value::Num(v);
            let args = Value::obj([
                ("device", "disk0".into()),
                ("bps", n(0.0)),
                ("bps_rd", n(0.0)),
                ("bps_wr", n(bps)),
                ("iops", n(0.0)),
                ("iops_rd", n(0.0)),
                ("iops_wr", n(0.0)),
            ]);
            qmp.execute("block_set_io_throttle", Some(args)).map_err(|e| (-32000, e))
        }
        // Every place guest RAM holds a byte pattern (TASK-101: no key after logout).
        "Freally.scanMemory" => scan_memory(qmp, shot_dir, params).map_err(|e| (-32000, e)),
        "Freally.quit" => {
            let _ = qmp.execute("quit", None);
            Ok(Value::Obj(Vec::new()))
        }
        m if cdpd_serves(m) || m.starts_with("Freally.") || m == "Target.activateTarget" => {
            Err((-32601, format!("{m} needs the in-OS test door (cdpd), in e2e images only")))
        }
        m => Err((-32601, format!("{m} isn't served"))),
    }
}

/// Where guest RAM holds `pattern` (hex, 8 bytes or more): QMP
/// `pmemsave` in 64 MiB pieces, each overlapping the last by the pattern's
/// length less one, so a match across two pieces is found once. q35 keeps
/// RAM at 0 (up to 2 GiB when there's more than 2.75 GiB), the rest from 4 GiB.
fn scan_memory(qmp: &mut Qmp, dir: &Path, params: &Value) -> Result<Value, String> {
    let hex = params.get("pattern").and_then(Value::as_str).ok_or("no pattern")?;
    let pattern = unhex(hex).ok_or("the pattern isn't hex")?;
    if pattern.len() < 8 {
        return Err("a pattern this short would match by chance".into());
    }
    let summary = qmp.execute("query-memory-size-summary", None)?;
    let size = summary.get("base-memory").and_then(Value::as_i64).ok_or("no base-memory")? as u64;
    let low = if size > 0xB000_0000 { 0x8000_0000 } else { size };
    let path = dir.join(format!("ram-{}.bin", std::process::id()));
    const PIECE: u64 = 64 << 20;
    let back = pattern.len() as u64 - 1;
    let mut at = Vec::new();
    for (start, len) in [(0u64, low), (1u64 << 32, size - low)] {
        let mut done = 0;
        while done < len {
            let from = start + done - if done > 0 { back } else { 0 };
            let end = start + (done + PIECE).min(len);
            let args = Value::obj([
                ("val", from.into()),
                ("size", (end - from).into()),
                ("filename", path.display().to_string().into()),
            ]);
            qmp.execute("pmemsave", Some(args))?;
            let bytes = std::fs::read(&path).map_err(|e| format!("pmemsave: {e}"))?;
            at.extend(
                bytes.windows(pattern.len()).enumerate().filter(|(_, w)| *w == pattern).map(|(i, _)| from + i as u64),
            );
            done = end - start;
        }
    }
    let _ = std::fs::remove_file(&path);
    let shown: Vec<Value> = at.iter().take(16).map(|a| Value::from(format!("{a:#x}"))).collect();
    Ok(Value::obj([("found", at.len().into()), ("at", Value::Arr(shown)), ("scanned", size.into())]))
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

pub fn desktop_target() -> Value {
    Value::obj([
        ("targetId", "desktop".into()),
        ("type", "page".into()),
        ("title", "Freally OS".into()),
        ("url", "freally://desktop".into()),
    ])
}

/// QMP `screendump` (PPM), cropped to `clip` if given, as a base64 PNG.
fn screenshot(qmp: &mut Qmp, dir: &Path, params: &Value) -> Result<Value, String> {
    if params.get("format").and_then(Value::as_str).is_some_and(|f| f != "png") {
        return Err("only png".into());
    }
    let path = dir.join(format!("shot-{}.ppm", std::process::id()));
    qmp.execute("screendump", Some(Value::obj([("filename", path.display().to_string().into())])))?;
    let data = std::fs::read(&path).map_err(|e| format!("screendump: {e}"))?;
    let _ = std::fs::remove_file(&path);
    let (w, h, rgb) = fimage::ppm::parse(&data)?;
    let (mut x0, mut y0, mut cw, mut ch) = (0u32, 0u32, w, h);
    if let Some(c) = params.get("clip") {
        let num = |k: &str| c.get(k).and_then(Value::as_f64).unwrap_or(0.0).max(0.0) as u32;
        x0 = num("x").min(w);
        y0 = num("y").min(h);
        cw = num("width").min(w - x0);
        ch = num("height").min(h - y0);
    }
    let mut out = Vec::with_capacity((cw * ch * 3) as usize);
    for y in y0..y0 + ch {
        let row = (y * w * 3) as usize;
        out.extend_from_slice(&rgb[row + (x0 * 3) as usize..row + ((x0 + cw) * 3) as usize]);
    }
    let png = fimage::png::encode(cw, ch, 3, &out);
    Ok(Value::obj([("data", fjson::base64::encode(&png).into())]))
}

use cdp_proto::qmp::qcode;

fn key(qmp: &mut Qmp, p: &Value) -> Result<Value, String> {
    let kind = p.get("type").and_then(Value::as_str).unwrap_or("");
    let down = match kind {
        "keyDown" | "rawKeyDown" => true,
        "keyUp" => false,
        "char" => return Ok(Value::Obj(Vec::new())), // text arrives with keyDown; cdpd types exact text
        _ => return Err(format!("unknown key event type {kind:?}")),
    };
    let code = p.get("code").and_then(Value::as_str).or_else(|| p.get("key").and_then(Value::as_str)).unwrap_or("");
    let q = qcode(code).ok_or_else(|| format!("no qcode for {code:?}"))?;
    let ev = Value::obj([
        ("type", "key".into()),
        (
            "data",
            Value::obj([("down", down.into()), ("key", Value::obj([("type", "qcode".into()), ("data", q.into())]))]),
        ),
    ]);
    qmp.execute("input-send-event", Some(Value::obj([("events", Value::Arr(vec![ev]))])))?;
    Ok(Value::Obj(Vec::new()))
}

/// Relative motion and buttons (the PS/2 mouse path; no absolute tablet).
fn mouse(qmp: &mut Qmp, p: &Value) -> Result<Value, String> {
    let kind = p.get("type").and_then(Value::as_str).unwrap_or("");
    let mut events = Vec::new();
    let button = match p.get("button").and_then(Value::as_str).unwrap_or("none") {
        "left" => Some("left"),
        "right" => Some("right"),
        "middle" => Some("middle"),
        _ => None,
    };
    match kind {
        "mouseMoved" => {
            for (axis, k) in [("x", "deltaX"), ("y", "deltaY")] {
                let d = p.get(k).and_then(Value::as_f64).unwrap_or(0.0) as i64;
                if d != 0 {
                    events.push(Value::obj([
                        ("type", "rel".into()),
                        ("data", Value::obj([("axis", axis.into()), ("value", d.into())])),
                    ]));
                }
            }
        }
        "mousePressed" | "mouseReleased" => {
            let b = button.ok_or("which button?")?;
            events.push(Value::obj([
                ("type", "btn".into()),
                ("data", Value::obj([("down", (kind == "mousePressed").into()), ("button", b.into())])),
            ]));
        }
        "mouseWheel" => {
            let dy = p.get("deltaY").and_then(Value::as_f64).unwrap_or(0.0);
            let b = if dy < 0.0 { "wheel-up" } else { "wheel-down" };
            for down in [true, false] {
                events.push(Value::obj([
                    ("type", "btn".into()),
                    ("data", Value::obj([("down", down.into()), ("button", b.into())])),
                ]));
            }
        }
        _ => return Err(format!("unknown mouse event type {kind:?}")),
    }
    if !events.is_empty() {
        qmp.execute("input-send-event", Some(Value::obj([("events", Value::Arr(events))])))?;
    }
    Ok(Value::Obj(Vec::new()))
}
