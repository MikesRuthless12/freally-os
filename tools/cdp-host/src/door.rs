// SPDX-License-Identifier: Apache-2.0
//! The test door on COM3 (e2e images): conductor answers each command line
//! with lines ending `end`; `cdpd <Method> <params as JSON>` is answered
//! `json <result>` by cdpd (TASK-063). One command at a time, each on its own
//! connection, as cdp-runner's own door calls are.

use fjson::Value;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct Door {
    addr: String,
    one: Mutex<()>,
}

impl Door {
    pub fn new(addr: String) -> Door {
        Door { addr, one: Mutex::new(()) }
    }

    /// One command; its answer's lines (before `end`).
    pub fn line(&self, cmd: &str) -> Result<Vec<String>, String> {
        let _one = self.one.lock().unwrap_or_else(|p| p.into_inner());
        let start = Instant::now();
        let stream = loop {
            match TcpStream::connect(&self.addr) {
                Ok(s) => break s,
                Err(e) if start.elapsed() > Duration::from_secs(10) => return Err(format!("the test door: {e}")),
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        };
        stream.set_read_timeout(Some(Duration::from_secs(15))).map_err(|e| e.to_string())?;
        let mut w = stream.try_clone().map_err(|e| e.to_string())?;
        w.write_all(format!("{cmd}\n").as_bytes()).map_err(|e| e.to_string())?;
        let mut lines = Vec::new();
        for line in BufReader::new(stream).lines() {
            let line = line.map_err(|e| format!("the test door went quiet: {e}"))?;
            let line = line.trim();
            if line == "end" {
                return Ok(lines);
            }
            if !line.is_empty() {
                lines.push(line.to_string());
            }
        }
        Err(String::from("the test door closed"))
    }

    /// One CDP method cdpd serves.
    pub fn cdpd(&self, method: &str, params: &Value) -> Result<Value, String> {
        let lines = self.line(&format!("cdpd {method} {}", params.to_json()))?;
        for l in &lines {
            if let Some(j) = l.strip_prefix("json ") {
                return fjson::parse(j).map_err(|e| format!("cdpd's answer at {}: {}", e.at, e.what));
            }
            if let Some(e) = l.strip_prefix("error: ") {
                return Err(e.to_string());
            }
        }
        Err(format!("cdpd said {lines:?}"))
    }
}
