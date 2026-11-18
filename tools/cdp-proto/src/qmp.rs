// SPDX-License-Identifier: Apache-2.0
//! A QMP client (QEMU's machine protocol: JSON lines on a Unix socket).

use fjson::Value;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

pub struct Qmp {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Qmp {
    /// Connects (waiting up to `wait` for QEMU to create the socket) and negotiates.
    pub fn connect(path: &Path, wait: Duration) -> Result<Qmp, String> {
        let start = Instant::now();
        let stream = loop {
            match UnixStream::connect(path) {
                Ok(s) => break s,
                Err(e) if start.elapsed() < wait => {
                    let _ = e;
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(format!("can't reach QMP at {}: {e}", path.display())),
            }
        };
        let writer = stream.try_clone().map_err(|e| e.to_string())?;
        let mut q = Qmp { reader: BufReader::new(stream), writer };
        let greeting = q.line()?;
        if greeting.get("QMP").is_none() {
            return Err("no QMP greeting".into());
        }
        q.execute("qmp_capabilities", None)?;
        Ok(q)
    }

    fn line(&mut self) -> Result<Value, String> {
        let mut s = String::new();
        if self.reader.read_line(&mut s).map_err(|e| e.to_string())? == 0 {
            return Err("QMP closed".into());
        }
        fjson::parse(s.trim()).map_err(|e| format!("bad QMP line: {e:?}"))
    }

    /// Runs a command; events that arrive meanwhile are skipped.
    pub fn execute(&mut self, cmd: &str, args: Option<Value>) -> Result<Value, String> {
        let mut msg = Value::obj([("execute", cmd.into())]);
        if let Some(a) = args {
            msg.set("arguments", a);
        }
        writeln!(self.writer, "{}", msg.to_json()).map_err(|e| e.to_string())?;
        loop {
            let v = self.line()?;
            if let Some(r) = v.get("return") {
                return Ok(r.clone());
            }
            if let Some(e) = v.get("error") {
                return Err(format!("QMP {cmd}: {}", e.get("desc").and_then(Value::as_str).unwrap_or("error")));
            }
        }
    }
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

/// Presses and releases one key.
pub fn tap(q: &mut Qmp, qcode: &str) -> Result<(), String> {
    for down in [true, false] {
        let ev = Value::obj([
            ("type", "key".into()),
            (
                "data",
                Value::obj([
                    ("down", down.into()),
                    ("key", Value::obj([("type", "qcode".into()), ("data", qcode.into())])),
                ]),
            ),
        ]);
        q.execute("input-send-event", Some(Value::obj([("events", Value::Arr(vec![ev]))])))?;
    }
    Ok(())
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
