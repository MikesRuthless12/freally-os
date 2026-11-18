// SPDX-License-Identifier: Apache-2.0
//! The CDP methods cdp-host serves from QMP. Methods that need the in-OS test
//! door (`cdpd`, e2e only) say so until it exists (Phase 3, TASK-063).

use crate::qmp::Qmp;
use fjson::Value;
use std::path::Path;

/// Serves one CDP call. `Ok(result)` or `Err((code, message))`.
pub fn call(qmp: &mut Qmp, shot_dir: &Path, method: &str, params: &Value) -> Result<Value, (i64, String)> {
    match method {
        "Page.captureScreenshot" => screenshot(qmp, shot_dir, params).map_err(|e| (-32000, e)),
        "Input.dispatchKeyEvent" => key(qmp, params).map_err(|e| (-32000, e)),
        "Input.dispatchMouseEvent" => mouse(qmp, params).map_err(|e| (-32000, e)),
        "Target.getTargets" => Ok(Value::obj([("targetInfos", Value::Arr(vec![desktop_target()]))])),
        "Freally.quit" => {
            let _ = qmp.execute("quit", None);
            Ok(Value::Obj(Vec::new()))
        }
        m if m.starts_with("DOM.")
            || m.starts_with("Accessibility.")
            || m.starts_with("Freally.")
            || m == "Input.insertText" =>
        {
            Err((-32601, format!("{m} needs the in-OS test door (cdpd), which arrives in Phase 3")))
        }
        m => Err((-32601, format!("{m} isn't served"))),
    }
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

/// A CDP `code` (or `key`) as a QEMU qcode.
pub fn qcode(code: &str) -> Option<String> {
    let simple = [
        ("Enter", "ret"),
        ("Escape", "esc"),
        ("Backspace", "backspace"),
        ("Tab", "tab"),
        ("Space", "spc"),
        ("ArrowUp", "up"),
        ("ArrowDown", "down"),
        ("ArrowLeft", "left"),
        ("ArrowRight", "right"),
        ("ShiftLeft", "shift"),
        ("ShiftRight", "shift_r"),
        ("ControlLeft", "ctrl"),
        ("ControlRight", "ctrl_r"),
        ("AltLeft", "alt"),
        ("AltRight", "alt_r"),
        ("MetaLeft", "meta_l"),
        ("MetaRight", "meta_r"),
        ("Minus", "minus"),
        ("Equal", "equal"),
        ("BracketLeft", "bracket_left"),
        ("BracketRight", "bracket_right"),
        ("Backslash", "backslash"),
        ("Semicolon", "semicolon"),
        ("Quote", "apostrophe"),
        ("Backquote", "grave_accent"),
        ("Comma", "comma"),
        ("Period", "dot"),
        ("Slash", "slash"),
        ("Delete", "delete"),
        ("Insert", "insert"),
        ("Home", "home"),
        ("End", "end"),
        ("PageUp", "pgup"),
        ("PageDown", "pgdn"),
        ("CapsLock", "caps_lock"),
        ("PrintScreen", "print"),
        ("Pause", "pause"),
        ("ContextMenu", "menu"),
    ];
    if let Some((_, q)) = simple.iter().find(|(c, _)| *c == code) {
        return Some(q.to_string());
    }
    if let Some(l) = code.strip_prefix("Key")
        && l.len() == 1
        && l.as_bytes()[0].is_ascii_uppercase()
    {
        return Some(l.to_ascii_lowercase());
    }
    if let Some(d) = code.strip_prefix("Digit")
        && d.len() == 1
        && d.as_bytes()[0].is_ascii_digit()
    {
        return Some(d.to_string());
    }
    if let Some(n) = code.strip_prefix('F')
        && let Ok(n) = n.parse::<u8>()
        && (1..=12).contains(&n)
    {
        return Some(format!("f{n}"));
    }
    None
}

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

#[cfg(test)]
mod tests {
    use super::qcode;

    #[test]
    fn codes_map_to_qcodes() {
        assert_eq!(qcode("KeyA").as_deref(), Some("a"));
        assert_eq!(qcode("Digit7").as_deref(), Some("7"));
        assert_eq!(qcode("F12").as_deref(), Some("f12"));
        assert_eq!(qcode("Enter").as_deref(), Some("ret"));
        assert_eq!(qcode("MetaLeft").as_deref(), Some("meta_l"));
        assert_eq!(qcode("F13"), None);
        assert_eq!(qcode("KeyAA"), None);
    }
}
