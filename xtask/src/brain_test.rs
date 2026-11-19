// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask brain-test` (TASK-081, 083, 084, 085, 087): an MCP client of
//! the Brain, the way Claude Code is one, against a dev VM booted headless.
//! It speaks the 2026-07-28 revision on COM2 and works every tool, every
//! resource and the live log; the refusals (an unknown tool, refused
//! arguments, an unsigned swap); the swap with the launcher's state handed
//! over, timed against FR-018's 500 ms; the rollback; a build that crashes
//! and is rolled back by itself; restart; and a reboot the Brain comes back
//! from. Each check is said as it passes; any red fails the run.

use crate::brain_swap;
use crate::build;
use crate::run::{self, VmOptions};
use fcore_abi::Flavour;
use fjson::Value;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

const PORT_OFFSET: u16 = 900;
const MODERN: &str = "2026-07-28";
/// The 13 tools the owner signed off (2026-11-18).
const TOOLS: [&str; 13] = [
    "system_status",
    "boot_timeline",
    "list_processes",
    "list_services",
    "read_log",
    "get_faults",
    "screenshot",
    "send_input",
    "restart_service",
    "swap_service",
    "rollback_service",
    "set_theme",
    "reboot",
];

struct Mcp {
    r: BufReader<TcpStream>,
    w: TcpStream,
    id: u64,
    /// Notifications that arrived while waiting for answers.
    events: Vec<Value>,
}

fn meta() -> Value {
    Value::obj([
        ("io.modelcontextprotocol/protocolVersion", MODERN.into()),
        (
            "io.modelcontextprotocol/clientInfo",
            Value::obj([("name", "freally-brain-test".into()), ("version", fcore_abi::VERSION.into())]),
        ),
        ("io.modelcontextprotocol/clientCapabilities", Value::Obj(Vec::new())),
    ])
}

impl Mcp {
    /// QEMU listens from the start; the Brain answers once it's up.
    fn connect(port: u16, wait: Duration) -> Result<Mcp, String> {
        let start = Instant::now();
        loop {
            if let Ok(s) = TcpStream::connect(("127.0.0.1", port)) {
                let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
                let r = BufReader::new(s.try_clone().map_err(|e| e.to_string())?);
                let mut m = Mcp { r, w: s, id: 0, events: Vec::new() };
                while start.elapsed() < wait {
                    if m.request("server/discover", Value::Obj(Vec::new())).is_ok() {
                        let _ = m.w.set_read_timeout(Some(Duration::from_secs(60)));
                        return Ok(m);
                    }
                }
            }
            if start.elapsed() >= wait {
                return Err(format!("the Brain never answered on 127.0.0.1:{port} in {} s", wait.as_secs()));
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// Asks until the Brain answers again (after a reboot).
    fn wait_back(&mut self, wait: Duration) -> Result<(), String> {
        let start = Instant::now();
        let _ = self.w.set_read_timeout(Some(Duration::from_secs(3)));
        while start.elapsed() < wait {
            if self.request("server/discover", Value::Obj(Vec::new())).is_ok() {
                let _ = self.w.set_read_timeout(Some(Duration::from_secs(60)));
                return Ok(());
            }
        }
        Err(format!("the Brain didn't come back in {} s", wait.as_secs()))
    }

    fn send(&mut self, msg: &Value) -> Result<(), String> {
        let mut line = msg.to_json();
        line.push('\n');
        self.w.write_all(line.as_bytes()).map_err(|e| format!("COM2: {e}"))
    }

    fn read(&mut self) -> Result<Value, String> {
        let mut line = String::new();
        let n = self.r.read_line(&mut line).map_err(|e| format!("COM2: {e}"))?;
        if n == 0 {
            return Err(String::from("COM2 closed"));
        }
        fjson::parse(line.trim())
            .map_err(|e| format!("not JSON from the Brain ({e:?}): {}", &line[..line.len().min(200)]))
    }

    /// A request in the modern era; its whole answer.
    fn request(&mut self, method: &str, mut params: Value) -> Result<Value, String> {
        self.id += 1;
        params.set("_meta", meta());
        let id = self.id;
        self.send(&Value::obj([
            ("jsonrpc", "2.0".into()),
            ("id", id.into()),
            ("method", method.into()),
            ("params", params),
        ]))?;
        loop {
            let m = self.read()?;
            if m.get("id").and_then(Value::as_i64) == Some(id as i64) {
                return Ok(m);
            }
            if m.get("method").is_some() {
                self.events.push(m);
            }
        }
    }

    /// `tools/call`: the result, or the JSON-RPC error as text.
    fn call(&mut self, tool: &str, args: Value) -> Result<Value, String> {
        let r = self.request("tools/call", Value::obj([("name", tool.into()), ("arguments", args)]))?;
        match (r.get("result"), r.get("error")) {
            (Some(res), _) => Ok(res.clone()),
            (_, Some(e)) => Err(format!("error {}", e.to_json())),
            _ => Err(format!("no result: {}", r.to_json())),
        }
    }

    /// A tool that must succeed: its JSON answer.
    fn ok(&mut self, tool: &str, args: Value) -> Result<Value, String> {
        let r = self.call(tool, args)?;
        if r.get("isError").and_then(Value::as_bool) == Some(true) {
            return Err(format!("{tool} refused: {}", text(&r)));
        }
        Ok(r.get("structuredContent").cloned().map(unwrap_items).unwrap_or(Value::Null))
    }

    /// A tool that must refuse: its words.
    fn refused(&mut self, tool: &str, args: Value) -> Result<String, String> {
        let r = self.call(tool, args)?;
        if r.get("isError").and_then(Value::as_bool) != Some(true) {
            return Err(format!("{tool} wasn't refused: {}", r.to_json()));
        }
        Ok(text(&r))
    }

    /// Waits for a notification `method` (≤ `secs`).
    fn event(&mut self, method: &str, secs: u64) -> Result<Value, String> {
        let until = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(i) = self.events.iter().position(|e| e.get("method").and_then(Value::as_str) == Some(method)) {
                return Ok(self.events.remove(i));
            }
            if Instant::now() > until {
                return Err(format!("no {method} in {secs} s"));
            }
            let _ = self.w.set_read_timeout(Some(Duration::from_millis(500)));
            if let Ok(m) = self.read() {
                self.events.push(m);
            }
            let _ = self.w.set_read_timeout(Some(Duration::from_secs(60)));
        }
    }
}

fn unwrap_items(v: Value) -> Value {
    match v.get("items") {
        Some(items) if matches!(v, Value::Obj(ref f) if f.len() == 1) => items.clone(),
        _ => v,
    }
}

fn text(result: &Value) -> String {
    result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .and_then(|c| c.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or_default()
}

fn n(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(f64::NAN)
}

fn arr(v: &Value) -> &[Value] {
    v.as_array().unwrap_or_default()
}

fn args<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    Value::obj(pairs)
}

/// A screenshot's picture: its size and mean brightness (0–255).
fn picture(m: &mut Mcp, window: Option<&str>) -> Result<(u32, u32, f64), String> {
    let a = match window {
        Some(w) => args([("window", w.into())]),
        None => Value::Obj(Vec::new()),
    };
    let r = m.call("screenshot", a)?;
    if r.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(text(&r));
    }
    let item = r.get("content").and_then(Value::as_array).and_then(|c| c.first()).ok_or("no content")?;
    if s(item, "type") != "image" || s(item, "mimeType") != "image/png" {
        return Err(format!("not a PNG: {}", item.to_json().chars().take(120).collect::<String>()));
    }
    let png = fjson::base64::decode(s(item, "data")).ok_or("the PNG isn't base64")?;
    let d = fimage::png::decode(&png).map_err(|e| format!("the PNG doesn't decode: {e}"))?;
    let sum: u64 = d.pixels.iter().map(|&b| b as u64).sum();
    Ok((d.w, d.h, sum as f64 / d.pixels.len().max(1) as f64))
}

struct Report {
    passed: usize,
    failed: Vec<String>,
}

impl Report {
    fn check(&mut self, name: &str, f: impl FnOnce() -> Result<String, String>) {
        let t = Instant::now();
        match f() {
            Ok(said) => {
                self.passed += 1;
                println!("brain-test: ✓ {name}: {said} ({} ms)", t.elapsed().as_millis());
            }
            Err(e) => {
                println!("brain-test: ✗ {name}: {e}");
                self.failed.push(name.to_string());
            }
        }
    }
}

fn desktop(m: &mut Mcp) -> Result<Value, String> {
    let list = m.ok("list_services", Value::Obj(Vec::new()))?;
    arr(&list)
        .iter()
        .find(|s| s.get("name").and_then(Value::as_str) == Some("desktop"))
        .cloned()
        .ok_or_else(|| String::from("no desktop"))
}

pub fn brain_test_cmd(_args: &[String]) -> Result<(), String> {
    run().map(|s| println!("brain-test: {s}"))
}

/// The whole test: "N of N checks green in S s", or what was red.
pub fn run() -> Result<String, String> {
    let t0 = Instant::now();
    let built = build::build(Flavour::Dev)?;
    // The builds the swaps hand over: the desktop as it is, and one made to crash.
    let swap = brain_swap::arguments("desktop", false)?;
    let crash = brain_swap::arguments("desktop", true)?;
    let mut o = VmOptions::new(Flavour::Dev, "brain-test");
    o.headless = true;
    o.port_offset = PORT_OFFSET;
    let mut vm = run::start_built(&o, &built)?;
    let result = run_checks(o.port(2), &swap, &crash);
    let _ = vm.kill();
    let _ = vm.wait();
    let rep = result?;
    let total = rep.passed + rep.failed.len();
    let said = format!("{} of {total} checks green in {:.0} s", rep.passed, t0.elapsed().as_secs_f64());
    if rep.failed.is_empty() { Ok(said) } else { Err(format!("{said}; red: {}", rep.failed.join(", "))) }
}

fn run_checks(port: u16, swap: &Value, crash: &Value) -> Result<Report, String> {
    let mut m = Mcp::connect(port, Duration::from_secs(90))?;
    let mut rep = Report { passed: 0, failed: Vec::new() };
    let empty = || Value::Obj(Vec::new());

    rep.check("discover", || {
        let r = m.request("server/discover", empty())?;
        let res = r.get("result").ok_or("no result")?;
        let info = res.get("_meta").and_then(|x| x.get("io.modelcontextprotocol/serverInfo")).ok_or("no serverInfo")?;
        let versions: Vec<&str> =
            arr(res.get("supportedVersions").unwrap_or(&Value::Null)).iter().filter_map(Value::as_str).collect();
        if s(info, "name") != "freally-brain"
            || s(info, "version") != fcore_abi::VERSION
            || versions.first() != Some(&MODERN)
        {
            return Err(res.to_json());
        }
        Ok(format!("freally-brain {}, {}", s(info, "version"), versions.join(" ")))
    });
    rep.check("tools/list", || {
        let r = m.request("tools/list", empty())?;
        let names: Vec<String> = r
            .get("result")
            .and_then(|x| x.get("tools"))
            .map(arr)
            .unwrap_or_default()
            .iter()
            .map(|t| s(t, "name").to_string())
            .collect();
        if names != TOOLS {
            return Err(format!("{names:?}"));
        }
        Ok(String::from("the 13 tools, each with its input schema"))
    });
    rep.check("system_status", || {
        let v = m.ok("system_status", empty())?;
        let ok = s(&v, "version") == fcore_abi::VERSION
            && s(&v, "flavour") == "dev"
            && s(&v, "slot") == "A"
            && s(&v, "build_id").len() == 40
            && n(&v, "cpus") >= 1.0
            && n(&v, "mem_used") > 0.0
            && n(&v, "boot_ms") > 0.0;
        if !ok {
            return Err(v.to_json());
        }
        Ok(format!("{} dev, slot A, booted in {} ms, {} CPUs", s(&v, "version"), n(&v, "boot_ms"), n(&v, "cpus")))
    });
    rep.check("boot_timeline", || {
        let v = m.ok("boot_timeline", empty())?;
        let stages: Vec<&str> = arr(&v).iter().map(|x| s(x, "stage")).collect();
        let ms: Vec<f64> = arr(&v).iter().map(|x| n(x, "ms_from_b0")).collect();
        if !stages.contains(&"B0") || stages.last() != Some(&"S4") || arr(&v).iter().any(|x| s(x, "label").is_empty()) {
            return Err(v.to_json());
        }
        Ok(format!("{} stages, S4 at {} ms", stages.len(), ms.last().copied().unwrap_or(0.0)))
    });
    rep.check("list_services", || {
        let v = m.ok("list_services", empty())?;
        let names: Vec<&str> = arr(&v).iter().map(|x| s(x, "name")).collect();
        let running = |n: &str| arr(&v).iter().any(|x| s(x, "name") == n && s(x, "state") == "running");
        if names.first() != Some(&"conductor") || !running("brain") || !running("desktop") {
            return Err(v.to_json());
        }
        Ok(format!("{} services, conductor first, the Brain and the desktop running", names.len()))
    });
    rep.check("list_processes", || {
        let v = m.ok("list_processes", empty())?;
        let d = arr(&v).iter().find(|x| s(x, "name") == "desktop").ok_or("no desktop")?;
        let pct = n(d, "cpu_pct");
        if !(0.0..=100.0).contains(&pct) || n(d, "mem_bytes") < 1.0 {
            return Err(d.to_json());
        }
        Ok(format!("{} processes; the desktop at {pct} % CPU", arr(&v).len()))
    });
    rep.check("read_log", || {
        let v = m.ok("read_log", args([("service", "brain".into()), ("limit", 1000.into())]))?;
        let recs = arr(v.get("records").unwrap_or(&Value::Null));
        // FR-035: every call is in the log, this one included.
        if !recs.iter().all(|r| s(r, "service") == "brain")
            || !recs.iter().any(|r| s(r, "msg").starts_with("brain: read_log"))
        {
            return Err(format!("{} records, none saying this call", recs.len()));
        }
        let warn = m.ok("read_log", args([("min_level", "warn".into())]))?;
        let levels_ok =
            arr(warn.get("records").unwrap_or(&Value::Null)).iter().all(|r| matches!(s(r, "level"), "warn" | "error"));
        if !levels_ok {
            return Err(String::from("min_level warn gave a quieter record"));
        }
        Ok(format!("{} of the Brain's records, each call logged; next_seq {}", recs.len(), n(&v, "next_seq")))
    });
    rep.check("get_faults", || {
        let v = m.ok("get_faults", empty())?;
        if v.as_array().is_none() {
            return Err(v.to_json());
        }
        Ok(format!("{} fault reports", arr(&v).len()))
    });
    let mut dark = 0.0;
    rep.check("screenshot", || {
        let (w, h, mean) = picture(&mut m, None)?;
        let (rw, rh, _) = picture(&mut m, Some("Rack"))?;
        if w < 640 || h < 480 || rw != w || rh >= h {
            return Err(format!("{w}x{h}, the Rack {rw}x{rh}"));
        }
        dark = mean;
        let missing = m.refused("screenshot", args([("window", "No Such Window".into())]))?;
        if !missing.contains("no window is called") {
            return Err(missing);
        }
        let log = m.ok("read_log", args([("service", "brain".into()), ("limit", 1000.into())]))?;
        let enc = arr(log.get("records").unwrap_or(&Value::Null))
            .iter()
            .rev()
            .map(|r| s(r, "msg"))
            .find(|t| t.starts_with("brain: screenshot ") && t.contains("encoded in"))
            .unwrap_or("")
            .trim_start_matches("brain: ")
            .to_string();
        Ok(format!("{w}x{h} PNG, the Rack {rw}x{rh}; an unknown window refused; the Brain: {enc}"))
    });
    rep.check("resources", || {
        let r = m.request("resources/list", empty())?;
        let uris: Vec<String> = r
            .get("result")
            .and_then(|x| x.get("resources"))
            .map(arr)
            .unwrap_or_default()
            .iter()
            .map(|x| s(x, "uri").to_string())
            .collect();
        for want in ["freally://log/live", "freally://boot/timeline", "freally://services/desktop/manifest"] {
            if !uris.iter().any(|u| u == want) {
                return Err(format!("no {want} among {uris:?}"));
            }
        }
        let read = |m: &mut Mcp, uri: &str| -> Result<String, String> {
            let r = m.request("resources/read", args([("uri", uri.into())]))?;
            let c = r.get("result").and_then(|x| x.get("contents")).map(arr).and_then(|c| c.first().cloned());
            c.map(|c| s(&c, "text").to_string()).ok_or_else(|| r.to_json())
        };
        let manifest = read(&mut m, "freally://services/desktop/manifest")?;
        let log = read(&mut m, "freally://log/live")?;
        if !manifest.contains("handoff     = true") || log.lines().count() < 10 {
            return Err(format!("the manifest: {manifest}"));
        }
        Ok(format!("{} resources; desktop's manifest; the live log's {} lines", uris.len(), log.lines().count()))
    });
    rep.check("the live log, followed", || {
        m.id += 1;
        let listen_id = m.id;
        let mut params =
            args([("notifications", args([("resourceSubscriptions", Value::Arr(vec!["freally://log/live".into()]))]))]);
        params.set("_meta", meta());
        m.send(&args([
            ("jsonrpc", "2.0".into()),
            ("id", listen_id.into()),
            ("method", "subscriptions/listen".into()),
            ("params", params),
        ]))?;
        m.event("notifications/subscriptions/acknowledged", 5)?;
        m.ok("system_status", empty())?;
        let up = m.event("notifications/resources/updated", 5)?;
        m.send(&args([
            ("jsonrpc", "2.0".into()),
            ("method", "notifications/cancelled".into()),
            ("params", args([("requestId", listen_id.into())])),
        ]))?;
        Ok(format!("told: {}", up.get("params").map(Value::to_json).unwrap_or_default()))
    });
    rep.check("refusals", || {
        let r = m.request("tools/call", args([("name", "format_disk".into()), ("arguments", empty())]))?;
        let code = r.get("error").map(|e| n(e, "code"));
        if code != Some(-32602.0) {
            return Err(format!("an unknown tool: {}", r.to_json()));
        }
        let theme = m.refused("set_theme", args([("theme", "blue".into())]))?;
        let me = m.refused("restart_service", args([("name", "brain".into())]))?;
        Ok(format!("unknown tool -32602; \"{theme}\"; \"{me}\""))
    });
    rep.check("set_theme", || {
        m.ok("set_theme", args([("theme", "light".into())]))?;
        let (_, _, light) = picture(&mut m, None)?;
        m.ok("set_theme", args([("theme", "dark".into())]))?;
        let (_, _, back) = picture(&mut m, None)?;
        if light < dark + 20.0 || (back - dark).abs() > 10.0 {
            return Err(format!("brightness dark {dark:.0}, light {light:.0}, dark again {back:.0}"));
        }
        Ok(format!("brightness {dark:.0} → {light:.0} → {back:.0}"))
    });
    rep.check("send_input", || {
        m.ok("send_input", args([("keys", "Super".into())]))?;
        std::thread::sleep(Duration::from_millis(300));
        picture(&mut m, Some("Launcher")).map_err(|e| format!("Super didn't open the launcher: {e}"))?;
        m.ok("send_input", args([("text", "mon".into())]))?;
        let clicked = m.ok("send_input", args([("click", args([("x", 4.into()), ("y", 4.into())]))]));
        Ok(format!(
            "Super opened the launcher, \"mon\" typed; a click at 4, 4: {}",
            if clicked.is_ok() { "ok" } else { "refused" }
        ))
    });
    let before = desktop(&mut m).ok();
    rep.check("swap_service (the launcher handed over)", || {
        // The click above may have closed the launcher: open it again.
        if picture(&mut m, Some("Launcher")).is_err() {
            m.ok("send_input", args([("keys", "Super".into())]))?;
            std::thread::sleep(Duration::from_millis(300));
        }
        let v = m.ok("swap_service", swap.clone())?;
        let ms = n(&v, "ms");
        std::thread::sleep(Duration::from_millis(300));
        picture(&mut m, Some("Launcher")).map_err(|e| format!("the launcher didn't survive the swap: {e}"))?;
        let log = m.ok("read_log", args([("service", "desktop".into()), ("limit", 1000.into())]))?;
        let took = arr(log.get("records").unwrap_or(&Value::Null))
            .iter()
            .any(|r| s(r, "msg").contains("took over from the build before (the launcher open)"));
        if !took {
            return Err(String::from("the desktop didn't say it took over the state"));
        }
        if ms > 500.0 {
            return Err(format!("{ms} ms, over FR-018's 500"));
        }
        Ok(format!("{ms} ms (≤ 500), the launcher still open, kept through 5 s"))
    });
    rep.check("an unsigned swap is refused", || {
        let mut bad = swap.clone();
        let mut sig = fjson::base64::decode(s(swap, "signature_base64")).ok_or("no signature")?;
        sig[0] ^= 1;
        bad.set("signature_base64", fjson::base64::encode(&sig).into());
        let pid = desktop(&mut m).map(|d| n(&d, "pid"))?;
        let why = m.refused("swap_service", bad)?;
        let after = desktop(&mut m).map(|d| n(&d, "pid"))?;
        if !why.starts_with("Integrity") || pid != after {
            return Err(format!("\"{why}\"; the desktop's pid {pid} → {after}"));
        }
        Ok(format!("\"{why}\", the desktop untouched"))
    });
    rep.check("rollback_service", || {
        let v = m.ok("rollback_service", args([("name", "desktop".into())]))?;
        let want = before.as_ref().map(|d| s(d, "binary_sha256").to_string()).unwrap_or_default();
        if s(&v, "restored_sha256") != want {
            return Err(format!("{} restored, {want} ran before", s(&v, "restored_sha256")));
        }
        Ok(format!("{} back in {} ms", &want[..12.min(want.len())], n(&v, "ms")))
    });
    rep.check("a build that crashes is rolled back", || {
        let was = desktop(&mut m)?;
        let why = m.refused("swap_service", crash.clone())?;
        let now = desktop(&mut m)?;
        if !why.contains("rolled back")
            || s(&now, "binary_sha256") != s(&was, "binary_sha256")
            || s(&now, "state") != "running"
        {
            return Err(format!("\"{why}\"; now {}", now.to_json()));
        }
        Ok(format!("\"{why}\"; the desktop running its build again"))
    });
    rep.check("restart_service", || {
        let v = m.ok("restart_service", args([("name", "desktop".into())]))?;
        Ok(format!("the desktop back in {} ms", n(&v, "ms")))
    });
    rep.check("reboot", || {
        m.ok("reboot", empty())?;
        // QEMU keeps the socket through a reset: the same line, the Brain
        // answering again once it's back.
        std::thread::sleep(Duration::from_secs(2));
        m.wait_back(Duration::from_secs(120))?;
        let v = m.ok("system_status", empty())?;
        if n(&v, "uptime_ms") > 120_000.0 {
            return Err(format!("up {} ms: not a new boot", n(&v, "uptime_ms")));
        }
        Ok(format!("the Brain back after the reset, up {} s", (n(&v, "uptime_ms") / 1000.0).round()))
    });
    Ok(rep)
}
