// SPDX-License-Identifier: Apache-2.0
//! cdpd (TASK-063, FR-039): the CDP test door inside the OS, in e2e images
//! only. Conductor's test door hands it one method at a time (cdp-host sends
//! them over COM3); it answers from stage (the windows and the trees they
//! published), conductor (services, the boot, faults) and the logger.
//!
//! - `Target.getTargets`; `DOM.getDocument`, `querySelector(All)`,
//!   `getBoxModel`, `getAttributes`; `Accessibility.getFullAXTree`;
//! - `Log.read` (cdp-host turns it into `Log.entryAdded`);
//! - `Performance.getMetrics`;
//! - `Freally.getBootTimeline`, `getProcesses`, `getFaults`, `readBack`,
//!   `inventory`.
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use fcore_abi::{FErr, signal, topic};
use fjson::Value;
use fstd::ipc::{Channel, Client};
use fstd::time::{Duration, Instant};
use fwire::proto::{ax, cdpd as cd, conductor as c, logger as l, stage as st};
use svc_cdpd::dom::{self, Doc};

fstd::program!(run);

fn deadline() -> u64 {
    Instant::after(Duration::from_secs(2))
}

struct Cdpd {
    stage: Option<Client>,
    logger: Option<Client>,
    doc: Doc,
    /// Every control seen in any document built since cdpd started (the
    /// inventory's "shown": a launcher opened and closed between two specs
    /// is in it), by its name.
    seen: alloc::collections::BTreeMap<String, Value>,
}

impl Cdpd {
    fn stage(&mut self) -> Result<&mut Client, String> {
        if self.stage.is_none() {
            self.stage = fstd::service::connect("stage").ok().map(Client::new);
        }
        self.stage.as_mut().ok_or_else(|| String::from("stage isn't up"))
    }

    /// Stage's windows (connecting again, once, to a stage restarted since).
    fn windows(&mut self) -> Result<st::Windows, String> {
        let mut answer = Err(String::from("stage isn't up"));
        for _ in 0..2 {
            let Ok(cl) = self.stage() else { continue };
            match cl.call::<_, st::Windows>(st::LIST_WINDOWS, &(), Vec::new(), deadline()) {
                Ok((w, _)) => return Ok(w),
                Err(e) => {
                    self.stage = None;
                    answer = Err(format!("stage: {e:?}"));
                }
            }
        }
        answer
    }

    fn tree(&mut self, window_id: u32) -> Option<ax::Tree> {
        let cl = self.stage().ok()?;
        let (b, _) = cl.call::<_, st::AxBytes>(st::AX_TREE, &st::ById { window_id }, Vec::new(), deadline()).ok()?;
        if b.bytes.is_empty() { None } else { fwire::from_payload::<ax::Tree>(&b.bytes).ok() }
    }

    /// The document as it is now.
    fn rebuild(&mut self) -> Result<(), String> {
        let ws = self.windows()?;
        let trees: Vec<(u32, Option<ax::Tree>)> =
            ws.windows.iter().map(|w| (w.window_id, self.tree(w.window_id))).collect();
        self.doc = Doc::build(&ws.windows, |id| trees.iter().find(|(w, _)| *w == id).and_then(|(_, t)| t.clone()));
        for c in self.doc.inventory() {
            let s = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let id = if s("testid").is_empty() { s("label") } else { s("testid") };
            self.seen.entry(format!("{} {} {id}", s("owner"), s("role"))).or_insert(c);
        }
        Ok(())
    }

    /// The node `params.nodeId` names (rebuilding the document first when
    /// it's the root, so a query sees what's on screen now).
    fn node(&mut self, params: &Value) -> Result<usize, String> {
        let id = params.get("nodeId").and_then(Value::as_i64).unwrap_or(1);
        if id <= 1 {
            self.rebuild()?;
        }
        self.doc.get(id).map(|_| (id - 1) as usize).ok_or_else(|| format!("no node {id}"))
    }

    fn call(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        match method {
            "Target.getTargets" => Ok(dom::targets(&self.windows()?.windows)),
            "DOM.getDocument" => {
                self.rebuild()?;
                let depth = params.get("depth").and_then(Value::as_i64).unwrap_or(1);
                Ok(Value::obj([("root", self.doc.node_json(0, depth))]))
            }
            "DOM.querySelector" | "DOM.querySelectorAll" => {
                let from = self.node(params)?;
                let sel = params.get("selector").and_then(Value::as_str).ok_or("no selector")?;
                let found = self.doc.select(from, sel)?;
                let ids: Vec<Value> = found.iter().map(|&i| Value::from(i as u32 + 1)).collect();
                Ok(if method == "DOM.querySelector" {
                    Value::obj([("nodeId", ids.first().cloned().unwrap_or(Value::from(0u32)))])
                } else {
                    Value::obj([("nodeIds", Value::Arr(ids))])
                })
            }
            "DOM.getBoxModel" => {
                let i = self.node(params)?;
                Ok(self.doc.box_model(i))
            }
            "DOM.getAttributes" => {
                let i = self.node(params)?;
                let attrs = self.doc.nodes[i]
                    .attrs
                    .iter()
                    .flat_map(|(k, v)| [Value::from(k.as_str()), Value::from(v.as_str())]);
                Ok(Value::obj([("attributes", Value::Arr(attrs.collect()))]))
            }
            "Accessibility.getFullAXTree" => {
                self.rebuild()?;
                Ok(self.doc.ax_tree())
            }
            "Freally.inventory" => {
                self.rebuild()?;
                let seen = self.seen.values().cloned().collect();
                Ok(Value::obj([("nodes", Value::Arr(self.doc.inventory())), ("seen", Value::Arr(seen))]))
            }
            // The control under the pointer (a click's target), and the one
            // with the keyboard (a key's): the inventory's "worked" half.
            "Freally.nodeAt" => {
                let p = self.stage()?.call::<_, st::PointerAt>(st::POINTER, &(), Vec::new(), deadline());
                let (at, _) = p.map_err(|e| format!("stage: {e:?}"))?;
                self.rebuild()?;
                let node = self.doc.control_at(at.x, at.y).map_or(Value::Null, |i| self.doc.control(i));
                Ok(Value::obj([("node", node)]))
            }
            "Freally.focusedNode" => {
                self.rebuild()?;
                let node = self.doc.focused_control().map_or(Value::Null, |i| self.doc.control(i));
                Ok(Value::obj([("node", node)]))
            }
            "Freally.getProcesses" => {
                let s = services()?;
                let list = s
                    .iter()
                    .map(|v| {
                        Value::obj([
                            ("name", v.name.as_str().into()),
                            ("description", v.description.as_str().into()),
                            ("state", format!("{:?}", v.state).into()),
                            ("pid", v.pid.into()),
                            ("restarts", v.restarts.into()),
                            ("cpuNs", v.cpu_ns.into()),
                            ("memBytes", v.mem_bytes.into()),
                        ])
                    })
                    .collect();
                Ok(Value::obj([("processes", Value::Arr(list))]))
            }
            "Freally.getBootTimeline" => {
                let stages = timeline()?
                    .into_iter()
                    .map(|(id, ns)| {
                        Value::obj([
                            ("name", c::STAGES.get(id as usize).copied().unwrap_or("?").into()),
                            ("ms", Value::Num(ns as f64 / 1e6)),
                        ])
                    })
                    .collect();
                Ok(Value::obj([("stages", Value::Arr(stages))]))
            }
            "Freally.getFaults" => {
                let since = params.get("sinceSeq").and_then(Value::as_i64).unwrap_or(0).max(0) as u64;
                let faults = faults(since)?;
                let last = faults.last().map_or(since, |f| f.seq);
                let list = faults
                    .iter()
                    .map(|f| {
                        Value::obj([
                            ("seq", f.seq.into()),
                            ("service", f.service.as_str().into()),
                            ("kind", format!("{:?}", f.kind).into()),
                            ("message", f.message.as_str().into()),
                            ("restarted", f.restarted.into()),
                        ])
                    })
                    .collect();
                Ok(Value::obj([("faults", Value::Arr(list)), ("lastSeq", last.into())]))
            }
            "Performance.getMetrics" => {
                let stages = timeline()?;
                let ms = |name: &str| {
                    let id = c::STAGES.iter().position(|s| *s == name)? as u8;
                    stages.iter().find(|(s, _)| *s == id).map(|(_, ns)| *ns as f64 / 1e6)
                };
                let s = services()?;
                let mut mem = [0u8; 16];
                let _ = fstd::sys::system_info(topic::MEMORY, &mut mem);
                let word = |i: usize| u64::from_le_bytes(mem[i..i + 8].try_into().unwrap_or_default());
                let firmware = match (ms("F0"), ms("F1")) {
                    (Some(a), Some(b)) => b - a,
                    _ => 0.0,
                };
                let running = s.iter().filter(|v| v.state == c::ServiceState::Running).count();
                let metric = |name: &str, v: Value| Value::obj([("name", name.into()), ("value", v)]);
                Ok(Value::obj([(
                    "metrics",
                    Value::Arr(alloc::vec![
                        metric("BootMs", Value::Num(ms("S4").unwrap_or(0.0))),
                        metric("FirmwareMs", Value::Num(firmware)),
                        metric("ProcessCount", (running as u32).into()),
                        metric("MemUsed", word(8).saturating_sub(word(0)).into()),
                        metric("ServiceRestarts", s.iter().map(|v| v.restarts).sum::<u32>().into()),
                    ]),
                )]))
            }
            "Freally.readBack" => self.read_back(params),
            "Log.read" => self.log(params),
            m => Err(format!("{m} isn't served by cdpd")),
        }
    }

    /// `Freally.readBack {service, query}`: what the real service holds.
    fn read_back(&mut self, params: &Value) -> Result<Value, String> {
        let service = params.get("service").and_then(Value::as_str).unwrap_or("");
        let query = params.get("query").and_then(Value::as_str).unwrap_or("");
        let value: String = match (service, query) {
            ("stage", "theme") => format!("{:?}", self.windows()?.theme),
            ("stage", "scale") => self.windows()?.scale_pct.to_string(),
            ("stage", "focused") => {
                let ws = self.windows()?;
                ws.windows.iter().find(|w| w.focused).map_or(String::new(), |w| w.title.clone())
            }
            ("conductor", q) if q.starts_with("state ") => {
                let name = &q[6..];
                let s = services()?;
                s.iter()
                    .find(|v| v.name == name)
                    .map(|v| format!("{:?}", v.state))
                    .ok_or(format!("no service {name}"))?
            }
            _ => {
                return Err(format!(
                    "cdpd reads back stage theme|scale|focused and conductor `state <name>`, not {service} {query}"
                ));
            }
        };
        Ok(Value::obj([("value", value.into())]))
    }

    /// `Log.read {cursor, limit}`: records after `cursor`, and the next one.
    fn log(&mut self, params: &Value) -> Result<Value, String> {
        let cursor = params.get("cursor").and_then(Value::as_i64).unwrap_or(0).max(0) as u64;
        let limit = params.get("limit").and_then(Value::as_i64).unwrap_or(100).clamp(1, 500) as u16;
        let svc = params.get("source").and_then(Value::as_str).map(String::from);
        let min_level = match params.get("minLevel").and_then(Value::as_str) {
            Some("error") => Some(l::ERROR),
            Some("warning") => Some(l::WARN),
            Some("info") => Some(l::INFO),
            _ => None,
        };
        let read = l::Read { cursor, limit, filter: l::Filter { svc, min_level } };
        // A logger restarted since the last call: connect again, once.
        let mut answer = Err(String::from("the logger isn't up"));
        for _ in 0..2 {
            if self.logger.is_none() {
                self.logger = fstd::service::connect("logger").ok().map(Client::new);
            }
            let Some(cl) = self.logger.as_mut() else { continue };
            match cl.call::<_, l::Records>(l::READ, &read, Vec::new(), deadline()) {
                Ok((r, _)) => {
                    answer = Ok(r);
                    break;
                }
                Err(e) => {
                    self.logger = None;
                    answer = Err(format!("logger: {e:?}"));
                }
            }
        }
        let recs = answer?;
        let level = |lv: u8| match lv {
            l::ERROR => "error",
            l::WARN => "warning",
            l::INFO => "info",
            _ => "verbose",
        };
        let entries = recs
            .records
            .iter()
            .map(|r| {
                Value::obj([
                    ("seq", r.seq.into()),
                    ("source", r.svc.as_str().into()),
                    ("level", level(r.level).into()),
                    ("text", r.msg.as_str().into()),
                    ("timestamp", Value::Num(r.ts_ns as f64 / 1e6)),
                ])
            })
            .collect();
        // Where to go on: the next page, or past the last record given.
        let next = match (recs.next_cursor, recs.records.last()) {
            (0, Some(r)) => r.seq + 1,
            (0, None) => cursor,
            (n, _) => n,
        };
        Ok(Value::obj([("entries", Value::Arr(entries)), ("next", next.into()), ("newest", recs.newest.into())]))
    }
}

fn services() -> Result<Vec<c::ServiceInfo>, String> {
    fstd::service::ask::<_, c::Services>(c::LIST_SERVICES, &())
        .map(|s| s.services)
        .map_err(|e| format!("conductor: {e:?}"))
}

fn timeline() -> Result<Vec<(u8, u64)>, String> {
    fstd::service::ask::<_, c::BootTimeline>(c::BOOT_TIMELINE, &())
        .map(|t| t.stages)
        .map_err(|e| format!("conductor: {e:?}"))
}

fn faults(since: u64) -> Result<Vec<c::FaultReport>, String> {
    let mut out = Vec::new();
    let mut cursor = since;
    loop {
        let page = fstd::service::ask::<_, c::Faults>(c::FAULTS, &c::Page { cursor, limit: 32 })
            .map_err(|e| format!("conductor: {e:?}"))?;
        out.extend(page.reports);
        if page.next_cursor == 0 || page.next_cursor == cursor {
            return Ok(out);
        }
        cursor = page.next_cursor;
    }
}

/// Answers one channel's waiting calls; false once it's gone.
fn serve(d: &mut Cdpd, ch: &Channel, client: &str) -> bool {
    loop {
        let m = match ch.try_recv() {
            Ok(m) => m,
            Err(FErr::ShouldWait) => return true,
            Err(_) => return false,
        };
        let Ok((h, p)) = fwire::parse(&m.bytes) else { continue };
        let answer = if client != "conductor" {
            Err(String::from("cdpd answers conductor's test door only"))
        } else if h.ordinal != cd::CALL {
            Err(String::from("not served"))
        } else {
            fwire::from_payload::<cd::Call>(p).map_err(|e| String::from(e.describe())).and_then(|call| {
                let params = if call.params.trim().is_empty() {
                    Value::Obj(Vec::new())
                } else {
                    fjson::parse(&call.params).map_err(|e| format!("params: {e:?}"))?
                };
                d.call(&call.method, &params)
            })
        };
        let reply = match answer {
            Ok(v) => fwire::reply(h.txid, h.ordinal, &cd::Answer { result: v.to_json() }),
            Err(why) => fwire::error_reply(h.txid, h.ordinal, FErr::InvalidArgs as i32, &why),
        };
        if let Ok(r) = reply {
            let _ = ch.send(&r, Vec::new());
        }
    }
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    let Ok((listen, give)) = Channel::pair() else { return 1 };
    if fstd::service::register(give).is_err() {
        return 1;
    }
    fstd::info!("cdpd: ready");
    let mut d = Cdpd { stage: None, logger: None, doc: Doc::default(), seen: Default::default() };
    let mut clients: Vec<(Channel, String)> = Vec::new();
    loop {
        let mut items = alloc::vec![[listen.0.raw(), signal::READABLE]];
        items.extend(clients.iter().map(|(c, _)| [c.0.raw(), signal::READABLE | signal::PEER_CLOSED]));
        match fstd::sys::wait_many(&items, u64::MAX) {
            Ok(0) => {
                while let Ok(m) = listen.try_recv() {
                    if let Ok((h, p)) = fwire::parse(&m.bytes)
                        && h.ordinal == c::INCOMING
                        && let Ok(inc) = fwire::from_payload::<c::Incoming>(p)
                        && let Some(ch) = m.handles.into_iter().next()
                    {
                        clients.push((Channel(ch), inc.client.to_string()));
                    }
                }
            }
            Ok(k) => {
                let i = k - 1;
                let (ch, name) = &clients[i];
                if !serve(&mut d, ch, name) {
                    clients.remove(i);
                }
            }
            Err(_) => fstd::time::sleep(Duration::from_millis(10)),
        }
    }
}
