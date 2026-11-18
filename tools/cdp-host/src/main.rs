// SPDX-License-Identifier: Apache-2.0
//! cdp-host: Freally OS's Chrome DevTools Protocol endpoint (PRD § 4, *CDP endpoint*).
//!
//! `cdp-host --qmp <vm>/qmp.sock [--com3 HOST:PORT] [--port 9333]` listens on
//! 127.0.0.1 only: `GET /json/version`, `GET /json/list` (one `desktop`
//! target), and the WebSocket at `/devtools/page/<target>`, where
//! `{id, method, params}` gets `{id, result}` or `{id, error}`. Methods come
//! from QMP, which works against any flavour, release included, or, in e2e
//! images, from cdpd through the test door on COM3 (TASK-063). After
//! `Log.enable`, each answer is followed by the `Log.entryAdded` events
//! logged since. Never Playwright.

mod door;
mod methods;

use cdp_proto::qmp;
use cdp_proto::{Message, WebSocket, read_request, respond};
use fjson::Value;
use std::io::BufReader;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Shared {
    qmp: Mutex<qmp::Qmp>,
    shot_dir: PathBuf,
    port: u16,
    /// The test door (e2e images), and the log's cursor once `Log.enable`d.
    door: Option<door::Door>,
    log: Mutex<Option<u64>>,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    let Some(qmp_path) = arg("--qmp") else {
        eprintln!("cdp-host --qmp <vm>/qmp.sock [--com3 HOST:PORT] [--port 9333]");
        std::process::exit(2);
    };
    let port: u16 = arg("--port").and_then(|p| p.parse().ok()).unwrap_or(9333);
    let qmp_path = PathBuf::from(qmp_path);
    let shot_dir = qmp_path.parent().map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let qmp = match qmp::Qmp::connect(&qmp_path, Duration::from_secs(30)) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("cdp-host: {e}");
            std::process::exit(1);
        }
    };
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cdp-host: can't listen on 127.0.0.1:{port}: {e}");
            std::process::exit(1);
        }
    };
    println!("cdp-host: listening on 127.0.0.1:{port}, QMP {}", qmp_path.display());
    let door = arg("--com3").map(door::Door::new);
    let shared = Arc::new(Shared { qmp: Mutex::new(qmp), shot_dir, port, door, log: Mutex::new(None) });
    for stream in listener.incoming().flatten() {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            if let Err(e) = serve(stream, &shared) {
                eprintln!("cdp-host: connection: {e}");
            }
        });
    }
}

fn serve(stream: TcpStream, s: &Shared) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let req = read_request(&mut reader)?;
    let mut w = stream;
    let ws_url = format!("ws://127.0.0.1:{}/devtools/page/desktop", s.port);
    match req.path.as_str() {
        "/json/version" => {
            let v = Value::obj([
                ("Browser", format!("Freally OS cdp-host/{}", env!("CARGO_PKG_VERSION")).into()),
                ("Protocol-Version", "1.3".into()),
                ("webSocketDebuggerUrl", ws_url.into()),
            ]);
            respond(&mut w, "200 OK", "application/json", v.to_json().as_bytes())
        }
        "/json/list" | "/json" => {
            let mut t = methods::desktop_target();
            t.set("id", "desktop".into());
            t.set("webSocketDebuggerUrl", ws_url.into());
            respond(&mut w, "200 OK", "application/json", Value::Arr(vec![t]).to_json().as_bytes())
        }
        p if p.starts_with("/devtools/page/") => {
            let mut ws = WebSocket::accept(reader, &req)?;
            loop {
                let msg = match ws.recv() {
                    Ok(m) => m,
                    // The client went away without a close frame: that ends the session too.
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
                    Err(e) => return Err(e),
                };
                let text = match msg {
                    Message::Text(t) => t,
                    Message::Binary(_) => continue,
                    Message::Close => return Ok(()),
                };
                let reply = handle(s, &text);
                ws.send_text(&reply.to_json())?;
                for event in log_events(s) {
                    ws.send_text(&event.to_json())?;
                }
            }
        }
        _ => respond(&mut w, "404 Not Found", "text/plain", b"not found"),
    }
}

fn handle(s: &Shared, text: &str) -> Value {
    let msg = match fjson::parse(text) {
        Ok(m) => m,
        Err(e) => return error(Value::Null, -32700, &format!("parse error at {}: {}", e.at, e.what)),
    };
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        return error(id, -32600, "no method");
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Obj(Vec::new()));
    match method {
        "Log.enable" | "Log.disable" => {
            let mut log = s.log.lock().unwrap_or_else(|p| p.into_inner());
            *log = (method == "Log.enable").then_some(0);
            return Value::obj([("id", id), ("result", Value::Obj(Vec::new()))]);
        }
        _ => {}
    }
    let mut qmp = match s.qmp.lock() {
        Ok(q) => q,
        Err(p) => p.into_inner(),
    };
    match methods::call(&mut qmp, s.door.as_ref(), &s.shot_dir, method, &params) {
        Ok(result) => Value::obj([("id", id), ("result", result)]),
        Err((code, m)) => error(id, code, &m),
    }
}

/// After `Log.enable`: every record logged since the last answer, as
/// `Log.entryAdded` events (none without a door).
fn log_events(s: &Shared) -> Vec<Value> {
    let mut log = s.log.lock().unwrap_or_else(|p| p.into_inner());
    let (Some(cursor), Some(door)) = (*log, s.door.as_ref()) else { return Vec::new() };
    let Ok(r) = door.cdpd("Log.read", &Value::obj([("cursor", cursor.into()), ("limit", 500u32.into())])) else {
        return Vec::new();
    };
    if let Some(next) = r.get("next").and_then(Value::as_i64) {
        *log = Some(next.max(0) as u64);
    }
    r.get("entries")
        .and_then(Value::as_array)
        .unwrap_or_default()
        .iter()
        .map(|e| Value::obj([("method", "Log.entryAdded".into()), ("params", Value::obj([("entry", e.clone())]))]))
        .collect()
}

fn error(id: Value, code: i64, message: &str) -> Value {
    Value::obj([("id", id), ("error", Value::obj([("code", code.into()), ("message", message.into())]))])
}
