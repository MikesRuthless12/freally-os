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
