// SPDX-License-Identifier: Apache-2.0
//! brain (FR-035; dev images only, compiled out of release and e2e): the MCP
//! server Claude Code reaches on COM2 through `brain-bridge`.
//!
//! - The line: drv-uart16550 hands it COM2; newline-delimited JSON-RPC both
//!   ways (`svc_brain::transport`, `svc_brain::mcp`).
//! - The read tools (TASK-081) ask conductor (services, the boot, faults,
//!   manifests), the logger and stage (windows, screenshots), and the kernel
//!   (`system_info`).
//! - The action tools (TASK-084) go through stage (input, the theme) and
//!   conductor (restart, swap, rollback, reboot).
//! - Every call is logged: `brain: <tool> <arguments>` (FR-035).
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use fcore_abi::{FErr, signal, topic};
use fjson::Value;
use fstd::handle::Handle;
use fstd::ipc::{CallError, Channel, Client};
use fstd::time::{Duration, Instant};
use fwire::Wire;
use fwire::proto::{conductor as c, input as inp, logger as l, serial as s, stage as st};
use svc_brain::mcp::{self, Host, Server, ToolAnswer};
use svc_brain::tools::{self, ToolDef};
use svc_brain::transport::Lines;

fstd::program!(run);

fn deadline(secs: u64) -> u64 {
    Instant::after(Duration::from_secs(secs))
}

/// Calls a service, connecting (again, once) if it isn't connected or has
/// restarted since.
fn ask<Q: Wire, A: Wire>(slot: &mut Option<Client>, name: &str, ord: u32, q: &Q) -> Result<(A, Vec<Handle>), String> {
    let mut said = format!("{name} isn't up");
    for _ in 0..2 {
        if slot.is_none() {
            *slot = fstd::service::connect(name).ok().map(Client::new);
        }
        let Some(cl) = slot.as_mut() else { continue };
        match cl.call::<Q, A>(ord, q, Vec::new(), deadline(5)) {
            Ok(r) => return Ok(r),
            Err(CallError::Remote(_, why)) => return Err(format!("{name}: {why}")),
            Err(e) => {
                *slot = None;
                said = format!("{name}: {e:?}");
            }
        }
    }
    Err(said)
}

/// Conductor, through the service's own directory channel.
fn conductor<Q: Wire, A: Wire>(ord: u32, q: &Q) -> Result<A, String> {
    fstd::service::ask::<Q, A>(ord, q).map_err(|e| match e {
        CallError::Remote(_, why) => format!("conductor: {why}"),
        e => format!("conductor: {e:?}"),
    })
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Milliseconds with one decimal, from nanoseconds.
fn ms(ns: u64) -> Value {
    Value::Num(((ns + 50_000) / 100_000) as f64 / 10.0)
}

fn state_name(s: c::ServiceState) -> &'static str {
    match s {
        c::ServiceState::Waiting => "waiting",
        c::ServiceState::Starting => "starting",
        c::ServiceState::Running => "running",
        c::ServiceState::Restarting => "restarting",
        c::ServiceState::Held => "held",
        c::ServiceState::Stopped => "stopped",
    }
}

const LEVELS: [&str; 5] = ["trace", "debug", "info", "warn", "error"];

/// PRD § 3's stages, in words.
const STAGE_LABELS: [&str; 15] = [
    "Firmware reset end",
    "OS loader start",
    "Bootloader entry",
    "Image read",
    "Image verified",
    "Exit boot services",
    "Kernel entry",
    "Memory ready",
    "Interrupts and timers ready",
    "All CPUs online",
    "Conductor started",
    "Logger and devmgr ready",
    "Framebuffer and input drivers ready",
    "First desktop frame presented",
    "Desktop accepts input",
];

fn services() -> Result<Vec<c::ServiceInfo>, String> {
    conductor::<_, c::Services>(c::LIST_SERVICES, &()).map(|s| s.services)
}

fn timeline() -> Result<Vec<(u8, u64)>, String> {
    conductor::<_, c::BootTimeline>(c::BOOT_TIMELINE, &()).map(|t| t.stages)
}

fn stage_ns(stages: &[(u8, u64)], name: &str) -> Option<u64> {
    let id = c::STAGES.iter().position(|s| *s == name)? as u8;
    stages.iter().find(|(s, _)| *s == id).map(|(_, ns)| *ns)
}

fn timeline_json() -> Result<Value, String> {
    let list = timeline()?
        .into_iter()
        .map(|(id, ns)| {
            Value::obj([
                ("stage", c::STAGES.get(id as usize).copied().unwrap_or("?").into()),
                ("label", STAGE_LABELS.get(id as usize).copied().unwrap_or("?").into()),
                ("ms_from_b0", ms(ns)),
            ])
        })
        .collect();
    Ok(Value::Arr(list))
}

fn service_json(s: &c::ServiceInfo) -> Value {
    Value::obj([
        ("name", s.name.as_str().into()),
        ("description", s.description.as_str().into()),
        ("state", state_name(s.state).into()),
        ("pid", s.pid.into()),
        ("restarts", s.restarts.into()),
        ("started_ns", s.started_ns.into()),
        ("binary_sha256", hex(&s.binary_sha256).into()),
        ("cpu_ns", s.cpu_ns.into()),
        ("mem_bytes", s.mem_bytes.into()),
    ])
}

fn manifest(name: &str) -> Result<String, String> {
    conductor::<_, c::Text>(c::MANIFEST, &c::ByName { name: name.to_string() }).map(|t| t.text)
}

fn info_words<const N: usize>(t: u64) -> [u64; N] {
    let mut b = [0u8; 64];
    let _ = fstd::sys::system_info(t, &mut b);
    core::array::from_fn(|i| u64::from_le_bytes(b[8 * i..8 * i + 8].try_into().unwrap_or_default()))
}

struct Brain {
    logger: Option<Client>,
    stage: Option<Client>,
    /// The newest log record the live log's followers were told about.
    told: u64,
}

impl Brain {
    fn system_status(&self) -> Result<Value, String> {
        let img = fstd::machine::image().ok_or("the kernel doesn't say which image runs")?;
        let [free, total] = info_words::<2>(topic::MEMORY);
        let [cpus] = info_words::<1>(topic::CPU);
        let stages = timeline()?;
        let flavour = ["release", "dev", "e2e"].get(img.flavour as usize).copied().unwrap_or("?");
        let mut v = Value::obj([
            ("version", img.version.as_str().into()),
            ("build_id", img.build_hex().into()),
            ("flavour", flavour.into()),
            ("slot", img.slot_name().into()),
            ("uptime_ms", (Instant::now().as_ns() / 1_000_000).into()),
        ]);
        if let Some(s4) = stage_ns(&stages, "S4") {
            v.set("boot_ms", ms(s4));
        }
        if let (Some(f0), Some(f1)) = (stage_ns(&stages, "F0"), stage_ns(&stages, "F1")) {
            v.set("firmware_ms", ms(f1.saturating_sub(f0)));
        }
        v.set("cpus", cpus.into());
        v.set("mem_total", total.into());
        v.set("mem_used", total.saturating_sub(free).into());
        Ok(v)
    }

    /// Two samples 250 ms apart: each process's share of a CPU between them.
    fn processes(&self) -> Result<Value, String> {
        let before = services()?;
        let t0 = Instant::now().as_ns();
        fstd::time::sleep(Duration::from_millis(250));
        let after = services()?;
        let dt = Instant::now().as_ns().saturating_sub(t0).max(1);
        let list = after
            .iter()
            .filter(|p| p.pid != 0)
            .map(|p| {
                let was = before.iter().find(|q| q.pid == p.pid && q.name == p.name).map_or(p.cpu_ns, |q| q.cpu_ns);
                let tenths = p.cpu_ns.saturating_sub(was).saturating_mul(1000) / dt;
                Value::obj([
                    ("pid", p.pid.into()),
                    ("name", p.name.as_str().into()),
                    ("description", p.description.as_str().into()),
                    ("state", state_name(p.state).into()),
                    ("mem_bytes", p.mem_bytes.into()),
                    ("cpu_pct", Value::Num(tenths.min(1000) as f64 / 10.0)),
                ])
            })
            .collect();
        Ok(Value::Arr(list))
    }

    fn read_log(&mut self, args: &Value) -> Result<Value, String> {
        let since = args.get("since_seq").and_then(Value::as_i64).map(|n| n as u64);
        let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(100) as u16;
        let min_level = args.get("min_level").and_then(Value::as_str).and_then(|n| LEVELS.iter().position(|l| *l == n));
        let svc = args.get("service").and_then(Value::as_str).map(String::from);
        let read = l::Read {
            cursor: since.map_or(0, |n| n + 1),
            limit,
            filter: l::Filter { svc, min_level: min_level.map(|n| n as u8) },
        };
        let (recs, _) = ask::<_, l::Records>(&mut self.logger, "logger", l::READ, &read)?;
        let records: Vec<Value> = recs
            .records
            .iter()
            .map(|r| {
                Value::obj([
                    ("seq", r.seq.into()),
                    ("ms", ms(r.ts_ns)),
                    ("service", r.svc.as_str().into()),
                    ("level", LEVELS.get(r.level as usize).copied().unwrap_or("?").into()),
                    ("msg", r.msg.as_str().into()),
                ])
            })
            .collect();
        // Pass next_seq back as since_seq to read on.
        let next = recs.records.last().map_or(since.unwrap_or(0), |r| r.seq);
        Ok(Value::obj([("records", Value::Arr(records)), ("next_seq", next.into())]))
    }

    /// The live log's text: the newest 200 records, a line each.
    fn log_text(&mut self) -> Result<String, String> {
        let newest = self.newest_log()?;
        let read = l::Read {
            cursor: newest.saturating_sub(199).max(1),
            limit: 200,
            filter: l::Filter { svc: None, min_level: None },
        };
        let (recs, _) = ask::<_, l::Records>(&mut self.logger, "logger", l::READ, &read)?;
        let mut t = String::new();
        for r in &recs.records {
            let level = LEVELS.get(r.level as usize).copied().unwrap_or("?");
            t.push_str(&format!("{} {:.1} {} {level} {}\n", r.seq, r.ts_ns as f64 / 1e6, r.svc, r.msg));
        }
        Ok(t)
    }

    fn newest_log(&mut self) -> Result<u64, String> {
        let read = l::Read { cursor: 0, limit: 1, filter: l::Filter { svc: None, min_level: None } };
        ask::<_, l::Records>(&mut self.logger, "logger", l::READ, &read).map(|(r, _)| r.newest)
    }

    fn faults(&self, args: &Value) -> Result<Value, String> {
        let mut cursor = args.get("since_seq").and_then(Value::as_i64).unwrap_or(0).max(0) as u64;
        let mut out = Vec::new();
        loop {
            let page = conductor::<_, c::Faults>(c::FAULTS, &c::Page { cursor, limit: 32 })?;
            for f in &page.reports {
                out.push(Value::obj([
                    ("seq", f.seq.into()),
                    ("ms", ms(f.ts_ns)),
                    ("pid", f.pid.into()),
                    ("service", f.service.as_str().into()),
                    ("kind", format!("{:?}", f.kind).into()),
                    ("rip", format!("{:#x}", f.rip).into()),
                    ("addr", format!("{:#x}", f.addr).into()),
                    ("message", f.message.as_str().into()),
                    ("frames", Value::Arr(f.frames.iter().map(|a| format!("{a:#x}").into()).collect())),
                    ("restarted", f.restarted.into()),
                ]));
            }
            if page.next_cursor == 0 || page.next_cursor == cursor {
                return Ok(Value::Arr(out));
            }
            cursor = page.next_cursor;
        }
    }

    fn windows(&mut self) -> Result<st::Windows, String> {
        ask::<_, st::Windows>(&mut self.stage, "stage", st::LIST_WINDOWS, &()).map(|(w, _)| w)
    }

    /// The whole screen, or the window whose title is `window` (exactly,
    /// else the first that contains it, any case), as a PNG.
    fn screenshot(&mut self, args: &Value) -> Result<ToolAnswer, String> {
        let window_id = match args.get("window").and_then(Value::as_str) {
            None => None,
            Some(want) => {
                let ws = self.windows()?.windows;
                let lower = want.to_lowercase();
                let w = ws
                    .iter()
                    .find(|w| w.title == want)
                    .or_else(|| ws.iter().find(|w| w.title.to_lowercase().contains(&lower)))
                    .ok_or_else(|| {
                        let titles: Vec<&str> = ws.iter().map(|w| w.title.as_str()).collect();
                        format!("no window is called {want:?}; the windows: {}", titles.join(", "))
                    })?;
                Some(w.window_id)
            }
        };
        let (shot, hs) = ask::<_, st::Shot>(&mut self.stage, "stage", st::SCREENSHOT, &st::Screenshot { window_id })?;
        let vmo = hs.into_iter().next().ok_or("stage sent no picture")?;
        let n = shot.w as usize * shot.h as usize;
        let mut px = alloc::vec![0u8; n * 4];
        fstd::sys::vmo_read(vmo.raw(), 0, &mut px).map_err(|e| format!("the picture: {e:?}"))?;
        // 0x00RRGGBB, little-endian: B, G, R, 0 in memory.
        let rgb: Vec<u8> = px.as_chunks::<4>().0.iter().flat_map(|p| [p[2], p[1], p[0]]).collect();
        drop(px);
        let t0 = Instant::now().as_ns();
        let png = fimage::png::encode_deflated(shot.w, shot.h, 3, &rgb);
        fstd::info!(
            "brain: screenshot {}x{}, {} KiB as PNG, encoded in {} ms",
            shot.w,
            shot.h,
            png.len() / 1024,
            Instant::now().as_ns().saturating_sub(t0) / 1_000_000
        );
        Ok(ToolAnswer::png(fjson::base64::encode(&png)))
    }

    fn inject(&mut self, key: Option<inp::Key>, mouse: Option<inp::Mouse>) -> Result<(), String> {
        ask::<_, ()>(&mut self.stage, "stage", st::INJECT_INPUT, &st::Inject { key, mouse }).map(|_| ())
    }

    fn pointer(&mut self) -> Result<st::PointerAt, String> {
        ask::<_, st::PointerAt>(&mut self.stage, "stage", st::POINTER, &()).map(|(p, _)| p)
    }

    /// Text, a chord, then a click, whichever are given, in that order.
    fn send_input(&mut self, args: &Value) -> Result<Value, String> {
        let mut presses = Vec::new();
        if let Some(t) = args.get("text").and_then(Value::as_str) {
            presses.extend(svc_brain::input::text(t)?);
        }
        if let Some(k) = args.get("keys").and_then(Value::as_str) {
            presses.extend(svc_brain::input::chord(k)?);
        }
        for p in presses {
            let key = inp::Key { usage: p.usage, down: p.down, ch: p.ch, modifiers: p.modifiers };
            self.inject(Some(key), None)?;
        }
        if let Some(click) = args.get("click") {
            let x = click.get("x").and_then(Value::as_i64).unwrap_or(0) as i32;
            let y = click.get("y").and_then(Value::as_i64).unwrap_or(0) as i32;
            let button = match click.get("button").and_then(Value::as_str) {
                Some("right") => 2,
                Some("middle") => 4,
                _ => 1,
            };
            // Relative motion, as a mouse sends it; checked and corrected,
            // since the pointer stops at the screen's edge.
            for _ in 0..4 {
                let at = self.pointer()?;
                let (dx, dy) = (x - at.x, y - at.y);
                if dx == 0 && dy == 0 {
                    break;
                }
                let step = |d: i32| d.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                self.inject(None, Some(inp::Mouse { dx: step(dx), dy: step(dy), wheel: 0, buttons: 0 }))?;
            }
            let at = self.pointer()?;
            if (at.x, at.y) != (x, y) {
                return Err(format!("the pointer stopped at {}, {} (the screen's edge?)", at.x, at.y));
            }
            self.inject(None, Some(inp::Mouse { dx: 0, dy: 0, wheel: 0, buttons: button }))?;
            self.inject(None, Some(inp::Mouse { dx: 0, dy: 0, wheel: 0, buttons: 0 }))?;
        }
        Ok(Value::obj([("ok", true.into())]))
    }

    /// Waits (≤ 10 s) until `name` runs with a build `ok` accepts; the ms
    /// from `t0`, and its row.
    fn wait_up(
        &self,
        name: &str,
        t0: u64,
        ok: impl Fn(&c::ServiceInfo) -> bool,
    ) -> Result<(u64, c::ServiceInfo), String> {
        let until = Instant::after(Duration::from_secs(10));
        loop {
            if let Some(s) = services()?.into_iter().find(|s| s.name == name)
                && s.state == c::ServiceState::Running
                && ok(&s)
            {
                return Ok((Instant::now().as_ns().saturating_sub(t0) / 1_000_000, s));
            }
            if Instant::now().as_ns() > until {
                return Err(format!("{name} wasn't running again within 10 s"));
            }
            fstd::time::sleep(Duration::from_millis(10));
        }
    }

    fn row(&self, name: &str) -> Result<c::ServiceInfo, String> {
        services()?.into_iter().find(|s| s.name == name).ok_or_else(|| format!("no service is called {name}"))
    }

    fn restart(&mut self, args: &Value) -> Result<Value, String> {
        let name = args.get("name").and_then(Value::as_str).unwrap_or_default();
        let was = self.row(name)?;
        let t0 = Instant::now().as_ns();
        conductor::<_, ()>(c::RESTART, &c::ByName { name: name.to_string() })?;
        let (ms, _) = self.wait_up(name, t0, |s| s.restarts > was.restarts)?;
        Ok(Value::obj([("ok", true.into()), ("ms", ms.into())]))
    }

    /// The new build in, its state handed over; then 5 s watched, since a
    /// build that ends in them is rolled back (FR-018) and the answer says so.
    fn swap(&mut self, args: &Value) -> Result<Value, String> {
        let name = args.get("name").and_then(Value::as_str).unwrap_or_default();
        let field = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or_default();
        let build = fjson::base64::decode(field("binary_base64")).ok_or("binary_base64 isn't base64")?;
        let signature: [u8; 64] = fjson::base64::decode(field("signature_base64"))
            .and_then(|s| s.try_into().ok())
            .ok_or("signature_base64 isn't a 64-byte signature in base64")?;
        self.row(name)?;
        let vmo = fstd::service::sized_vmo(&build).map_err(|e| format!("no room for the build: {e:?}"))?;
        let t0 = Instant::now().as_ns();
        let swap = c::Swap { name: name.to_string(), binary: fwire::HandleRef(0), signature };
        let (sw, _) =
            fstd::service::ask_moving::<_, c::Swapped>(c::SWAP, &swap, alloc::vec![vmo], Duration::from_secs(15))
                .map_err(|e| match e {
                    CallError::Remote(code, why) if code == FErr::Integrity as i32 => format!("Integrity: {why}"),
                    CallError::Remote(_, why) => why,
                    e => format!("conductor: {e:?}"),
                })?;
        let (ms, up) = self.wait_up(name, t0, |s| s.binary_sha256 == sw.new_sha256)?;
        // The probation: one more restart means the new build ended and
        // conductor put the old one back (the hashes may match: a rebuild).
        let until = Instant::after(Duration::from_secs(5));
        while Instant::now().as_ns() < until {
            fstd::time::sleep(Duration::from_millis(100));
            if self.row(name)?.restarts > up.restarts {
                return Err(format!(
                    "the new build ({}) ended within 5 s and was rolled back: {} runs again",
                    hex(&sw.new_sha256[..6]),
                    hex(&sw.old_sha256[..6])
                ));
            }
        }
        fstd::info!("brain: {name} swapped in {ms} ms and kept");
        Ok(Value::obj([
            ("old_sha256", hex(&sw.old_sha256).into()),
            ("new_sha256", hex(&sw.new_sha256).into()),
            ("ms", ms.into()),
        ]))
    }

    fn rollback(&mut self, args: &Value) -> Result<Value, String> {
        let name = args.get("name").and_then(Value::as_str).unwrap_or_default();
        let t0 = Instant::now().as_ns();
        let r = conductor::<_, c::RolledBack>(c::ROLLBACK, &c::ByName { name: name.to_string() })?;
        let (ms, _) = self.wait_up(name, t0, |s| s.binary_sha256 == r.restored_sha256)?;
        Ok(Value::obj([("restored_sha256", hex(&r.restored_sha256).into()), ("ms", ms.into())]))
    }

    fn reboot(&mut self, args: &Value) -> Result<Value, String> {
        let other_slot = args.get("slot").and_then(Value::as_str) == Some("other");
        conductor::<_, ()>(c::REBOOT, &c::Reboot { other_slot })?;
        Ok(Value::obj([("ok", true.into())]))
    }

    fn set_theme(&mut self, args: &Value) -> Result<Value, String> {
        let theme = match args.get("theme").and_then(Value::as_str) {
            Some("light") => st::Theme::Light,
            _ => st::Theme::Dark,
        };
        ask::<_, ()>(&mut self.stage, "stage", st::SET_THEME, &st::SetTheme { theme })?;
        Ok(Value::obj([("ok", true.into())]))
    }
}

impl Host for Brain {
    fn call(&mut self, tool: &ToolDef, args: &Value) -> ToolAnswer {
        fstd::info!("{}", mcp::summary(tool.name, args));
        let answer = match tool.name {
            "system_status" => self.system_status().map(ToolAnswer::json),
            "boot_timeline" => timeline_json().map(ToolAnswer::json),
            "list_processes" => self.processes().map(ToolAnswer::json),
            "list_services" => services().map(|s| ToolAnswer::json(Value::Arr(s.iter().map(service_json).collect()))),
            "read_log" => self.read_log(args).map(ToolAnswer::json),
            "get_faults" => self.faults(args).map(ToolAnswer::json),
            "screenshot" => self.screenshot(args),
            "send_input" => self.send_input(args).map(ToolAnswer::json),
            "set_theme" => self.set_theme(args).map(ToolAnswer::json),
            "restart_service" => self.restart(args).map(ToolAnswer::json),
            "swap_service" => self.swap(args).map(ToolAnswer::json),
            "rollback_service" => self.rollback(args).map(ToolAnswer::json),
            "reboot" => self.reboot(args).map(ToolAnswer::json),
            other => Err(format!("{other} isn't a tool the Brain has")),
        };
        answer.unwrap_or_else(|why| {
            fstd::warn!("brain: {} refused: {why}", tool.name);
            ToolAnswer::error(&why)
        })
    }

    fn read(&mut self, uri: &str) -> Option<(&'static str, String)> {
        match uri {
            tools::LOG_LIVE => self.log_text().ok().map(|t| ("text/plain", t)),
            "freally://boot/timeline" => timeline_json().ok().map(|v| ("application/json", v.to_json())),
            _ => {
                let name = uri.strip_prefix("freally://services/")?.strip_suffix("/manifest")?;
                manifest(name).ok().map(|t| ("text/plain", t))
            }
        }
    }

    fn services(&mut self) -> Vec<String> {
        services().map(|s| s.into_iter().map(|s| s.name).collect()).unwrap_or_default()
    }
}

/// COM2, from drv-uart16550.
fn open_line() -> Option<Channel> {
    let mut uart = None;
    let (_, mut hs) = ask::<_, s::Opened>(&mut uart, "drv-uart16550", s::OPEN, &s::Open { port: 2 }).ok()?;
    (!hs.is_empty()).then(|| Channel(hs.remove(0)))
}

/// Sends what's waiting, as far as the driver takes it.
fn flush(port: &Channel, out: &mut Vec<u8>) -> Result<(), FErr> {
    while !out.is_empty() {
        let n = out.len().min(60_000);
        let m = fwire::event(s::WRITE, &s::Bytes { data: out[..n].to_vec() }).map_err(|_| FErr::Internal)?;
        match port.send(&m, Vec::new()) {
            Ok(()) => {
                out.drain(..n);
            }
            Err(FErr::Busy) => return Ok(()),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    // Registered so conductor counts it up; nothing connects to it.
    let Ok((_listen, give)) = Channel::pair() else { return 1 };
    if fstd::service::register(give).is_err() {
        return 1;
    }
    let version = fstd::machine::image().map_or_else(|| String::from(fcore_abi::VERSION), |i| i.version);
    let mut server = Server::new(&version);
    let mut brain = Brain { logger: None, stage: None, told: 0 };
    let mut lines = Lines::default();
    let mut out: Vec<u8> = Vec::new();
    let mut port: Option<Channel> = None;
    let mut dropped = 0;
    let mut following = false;
    loop {
        if port.is_none() {
            port = open_line();
            match &port {
                Some(_) => fstd::info!("brain: listening on COM2 (MCP {} and earlier)", mcp::MODERN),
                None => {
                    fstd::time::sleep(Duration::from_secs(1));
                    continue;
                }
            }
        }
        let Some(p) = port.as_ref() else { continue };
        let woke = fstd::sys::wait_many(&[[p.0.raw(), signal::READABLE | signal::PEER_CLOSED]], deadline(1));
        let now_ms = Instant::now().as_ns() / 1_000_000;
        if woke.is_ok() {
            loop {
                match p.try_recv() {
                    Ok(m) => {
                        if let Ok((h, body)) = fwire::parse(&m.bytes)
                            && h.ordinal == s::DATA
                            && let Ok(b) = fwire::from_payload::<s::Bytes>(body)
                        {
                            for line in lines.push(&b.data, now_ms) {
                                for reply in server.handle(&line, &mut brain) {
                                    out.extend_from_slice(reply.as_bytes());
                                    out.push(b'\n');
                                }
                            }
                        }
                    }
                    Err(FErr::ShouldWait) => break,
                    Err(_) => {
                        port = None;
                        break;
                    }
                }
            }
        }
        lines.expire(now_ms);
        if lines.dropped != dropped {
            dropped = lines.dropped;
            fstd::warn!("brain: dropped a line that was cut off or too long ({dropped} so far)");
        }
        // The live log: tell its followers when it grew (a new follower
        // starts from the log as it was when it subscribed).
        if server.following()
            && let Ok(newest) = brain.newest_log()
        {
            if following && newest > brain.told {
                for n in server.log_grew() {
                    out.extend_from_slice(n.as_bytes());
                    out.push(b'\n');
                }
            }
            brain.told = newest;
        }
        following = server.following();
        if let Some(p) = port.as_ref()
            && flush(p, &mut out).is_err()
        {
            port = None;
        }
    }
}
