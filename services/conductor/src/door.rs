// SPDX-License-Identifier: Apache-2.0
//! The test door (e2e flavour only, compiled out of every other): line
//! commands on COM3 for the CDP specs until cdpd exists (Phase 3), through
//! drv-uart16550's COM3 channel.
//!
//! - `kill <service>`: ends the service's process; when it registers again,
//!   the door says `back <service> <ms>`.
//! - `log <n>`: the logger's newest `n` records, one `rec <svc> <msg>` each.
//! - `services`: one `svc <name> <state>` per service.
//! - `devices`, `fbfill <rrggbb>`: devmgr's PCI list; a whole-screen fill.
//! - `bootctl`: the A/B record (`slot A Good 0.4.0`, `active A`, `booted A`);
//!   `bootctl trial b`: slot B on trial from the next boot (`@boot.fallback`).
//! - `stage …`: windows through stage's own protocol (`door_stage.rs`).
//! - `terminal …`: the terminal's screen, fed and read (`door_terminal.rs`).
//! - `monitor …`: what Monitor shows (`door_monitor.rs`).
//! - `cdpd <Method> <params as JSON>` → `json <result>` (one line): a CDP
//!   method cdpd serves (TASK-063), for cdp-host.
//! - `echo <n> [ms]`: `ok`, then the next `n` bytes that arrive go straight
//!   back. With `ms`, the door first stops reading for that long, so the
//!   driver's channel fills and it has to hold the line (`@serial.echo`).
//!
//! Each answer ends with `end`. While the driver is down (`@services.restart`
//! kills it too) answers wait in the outbox, and go out once it's back.

use crate::services::Conductor;
use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::{FErr, signal};
use fstd::ipc::{Channel, Client};
use fstd::serial_json::Framer;
use fstd::time::{Duration, Instant};
use fwire::proto::logger as l;
use fwire::proto::serial as s;

/// The most one `echo` takes.
const ECHO_MAX: usize = 64 << 20;

pub struct Door {
    /// drv-uart16550's COM3, once it's up.
    port: Option<Channel>,
    /// Command lines, framed as the Brain and cdpd frame theirs.
    lines: Framer,
    /// Answers not yet handed to the driver.
    out: Vec<u8>,
    /// Bytes still to echo.
    echo: usize,
    /// Not reading the port until then (ns): an echo's deliberate stall.
    stall_until: u64,
    /// The door's windows (`stage …`).
    stage: crate::door_stage::StageDoor,
    /// The terminal's test client (`terminal …`).
    terminal: crate::door_terminal::TermDoor,
    /// Monitor's (`monitor …`).
    monitor: crate::door_monitor::MonDoor,
    /// cdpd's client (`cdpd …`, TASK-063).
    cdpd: Option<Client>,
}

impl Door {
    pub fn open() -> Door {
        Door {
            port: None,
            lines: Framer::new(),
            out: Vec::new(),
            echo: 0,
            stall_until: 0,
            stage: crate::door_stage::StageDoor::default(),
            terminal: crate::door_terminal::TermDoor::default(),
            monitor: crate::door_monitor::MonDoor::default(),
            cdpd: None,
        }
    }

    /// The port's channel, for conductor's wait: it wakes when bytes arrive.
    pub fn wait_item(&self) -> Option<[u32; 2]> {
        if Instant::now().as_ns() < self.stall_until {
            return None;
        }
        self.port.as_ref().map(|p| [p.0.raw(), signal::READABLE | signal::PEER_CLOSED])
    }

    fn connect(&mut self, c: &mut Conductor) {
        if self.port.is_some() {
            return;
        }
        let Some(ch) = c.open_connection("drv-uart16550", "test-door") else { return };
        let deadline = Instant::after(Duration::from_secs(1));
        if let Ok((_, mut hs)) =
            Client::new(ch).call::<_, s::Opened>(s::OPEN, &s::Open { port: 3 }, Vec::new(), deadline)
            && !hs.is_empty()
        {
            self.port = Some(Channel(hs.remove(0)));
            fstd::info!("conductor: test door open on COM3");
        }
    }

    fn send(&mut self, line: &str) {
        self.out.extend_from_slice(line.as_bytes());
        self.out.push(b'\n');
    }

    /// One CDP method through cdpd: `json <result>`, or `error: <why>`.
    fn cdpd(&mut self, c: &mut Conductor, arg: &str) -> String {
        use fwire::proto::cdpd as cd;
        let (method, params) = arg.split_once(' ').unwrap_or((arg, "{}"));
        let call = cd::Call { method: String::from(method), params: String::from(params) };
        let mut said = String::from("error: cdpd isn't up");
        // A cdpd restarted since the last call: connect to the new one, once.
        for _ in 0..2 {
            // cdpd asks conductor (the services, the boot, connections) while
            // conductor waits for its answer, and so may any app cdpd asks in
            // turn: serve every service's requests meanwhile.
            let Some(i) = c.services.iter().position(|s| s.m.name == "cdpd") else {
                return String::from("error: no cdpd in this image");
            };
            if c.services[i].child.is_none() {
                return String::from("error: cdpd isn't running");
            }
            let (who, dirs): (Vec<usize>, Vec<u32>) = c
                .services
                .iter()
                .enumerate()
                .filter_map(|(k, s)| s.child.as_ref().map(|ch| (k, ch.channel.0.raw())))
                .unzip();
            if self.cdpd.is_none() {
                self.cdpd = c.open_connection("cdpd", "conductor").map(fstd::ipc::Client::new);
            }
            let Some(cl) = self.cdpd.as_mut() else { continue };
            let deadline = Instant::after(Duration::from_secs(10));
            match cl.call_serving::<_, cd::Answer>(cd::CALL, &call, deadline, &dirs, |k| c.request(who[k])) {
                Ok((a, _)) => return alloc::format!("json {}", a.result),
                Err(fstd::ipc::CallError::Remote(_, why)) => return alloc::format!("error: {why}"),
                Err(e) => {
                    self.cdpd = None;
                    said = alloc::format!("error: {e:?}");
                }
            }
        }
        said
    }

    /// Hands the outbox to the driver, as far as it takes it.
    fn flush(&mut self) {
        while !self.out.is_empty() {
            let Some(port) = &self.port else { return };
            let n = self.out.len().min(60_000);
            let Ok(m) = fwire::event(s::WRITE, &s::Bytes { data: self.out[..n].to_vec() }) else { return };
            match port.send(&m, Vec::new()) {
                Ok(()) => {
                    self.out.drain(..n);
                }
                Err(FErr::Busy) => return,
                Err(_) => {
                    self.port = None;
                    return;
                }
            }
        }
    }

    /// Reads what has arrived; runs each whole line.
    pub fn poll(&mut self, c: &mut Conductor) {
        self.connect(c);
        for out in core::mem::take(&mut c.door_out) {
            self.send(&out);
            self.send("end");
        }
        while let Some(port) = &self.port
            && Instant::now().as_ns() >= self.stall_until
        {
            match port.try_recv() {
                Ok(m) => {
                    if let Ok((h, p)) = fwire::parse(&m.bytes)
                        && h.ordinal == s::DATA
                        && let Ok(b) = fwire::from_payload::<s::Bytes>(p)
                    {
                        self.input(c, &b.data);
                    }
                }
                Err(FErr::ShouldWait) => break,
                Err(_) => {
                    self.port = None;
                    break;
                }
            }
        }
        self.flush();
    }

    fn input(&mut self, c: &mut Conductor, mut data: &[u8]) {
        while !data.is_empty() {
            if self.echo > 0 {
                let n = self.echo.min(data.len());
                self.out.extend_from_slice(&data[..n]);
                data = &data[n..];
                self.echo -= n;
                if self.echo == 0 {
                    self.send("end");
                }
                self.flush();
                continue;
            }
            let (line, used) = self.lines.next(data, Instant::now().as_ns());
            data = &data[used..];
            // A dropped line (abandoned, too long, not UTF-8) is nothing: the
            // client's request times out.
            if let Some(Ok(cmd)) = line {
                self.run(c, cmd.trim());
            }
        }
    }

    fn run(&mut self, c: &mut Conductor, cmd: &str) {
        let (verb, arg) = cmd.split_once(' ').unwrap_or((cmd, ""));
        match verb {
            // The kernel panics on purpose (`kernel panic`, TASK-074): the
            // next boot must show it in Monitor → Faults. Never answered.
            "kernel" if arg == "panic" => {
                let _ = fstd::sys::call1(fcore_abi::syscall::TEST_PANIC, [0; 6]);
                self.send("error: the kernel didn't panic");
            }
            "kill" => match c.services.iter_mut().find(|s| s.m.name == arg) {
                Some(s) if s.child.is_some() => {
                    s.killed_at = Some(Instant::now().as_ns());
                    let ok = s.child.as_ref().is_some_and(|ch| fstd::sys::process_kill(ch.process.raw()).is_ok());
                    // Killing the door's own carrier: what's said next waits for its return.
                    if arg == "drv-uart16550" {
                        self.port = None;
                    }
                    self.send(if ok { "ok" } else { "error: kill failed" });
                    // `back` follows when it registers again.
                    return;
                }
                _ => self.send("error: no such running service"),
            },
            "log" => {
                let n: u16 = arg.parse().unwrap_or(10);
                match c.open_connection("logger", "test-door") {
                    Some(ch) => {
                        let mut client = fstd::ipc::Client::new(ch);
                        let read = l::Read { cursor: 0, limit: n, filter: l::Filter { svc: None, min_level: None } };
                        match client.call::<_, l::Records>(
                            l::READ,
                            &read,
                            Vec::new(),
                            Instant::after(Duration::from_secs(2)),
                        ) {
                            Ok((recs, _)) => {
                                for r in recs.records {
                                    self.send(&alloc::format!("rec {} {}", r.svc, r.msg));
                                }
                            }
                            Err(e) => self.send(&alloc::format!("error: {e:?}")),
                        }
                    }
                    None => self.send("error: the logger isn't ready"),
                }
            }
            "devices" => match c.open_connection("devmgr", "test-door") {
                Some(ch) => {
                    let mut client = fstd::ipc::Client::new(ch);
                    match client.call::<_, fwire::proto::devmgr::Devices>(
                        fwire::proto::devmgr::LIST,
                        &(),
                        Vec::new(),
                        Instant::after(Duration::from_secs(2)),
                    ) {
                        Ok((list, _)) => {
                            for p in list.pci {
                                self.send(&alloc::format!(
                                    "pci {:02x}:{:02x}.{} {:04x}:{:04x} {:02x}.{:02x}.{:02x}",
                                    p.bus,
                                    p.device,
                                    p.function,
                                    p.vendor,
                                    p.id,
                                    p.class,
                                    p.subclass,
                                    p.prog_if
                                ));
                            }
                        }
                        Err(e) => self.send(&alloc::format!("error: {e:?}")),
                    }
                }
                None => self.send("error: devmgr isn't ready"),
            },
            "fbfill" => {
                let rgb = u32::from_str_radix(arg, 16).unwrap_or(0);
                match c.open_connection("drv-gop-fb", "test-door") {
                    Some(ch) => {
                        let mut client = fstd::ipc::Client::new(ch);
                        match client.call::<_, ()>(
                            fwire::proto::display::FILL,
                            &fwire::proto::display::Fill { rgb },
                            Vec::new(),
                            Instant::after(Duration::from_secs(2)),
                        ) {
                            Ok(_) => self.send("ok"),
                            Err(e) => self.send(&alloc::format!("error: {e:?}")),
                        }
                    }
                    None => self.send("error: drv-gop-fb isn't ready"),
                }
            }
            "echo" => match echo_args(arg) {
                Some((n, stall_ms)) if n <= ECHO_MAX && stall_ms <= 60_000 => {
                    self.send("ok");
                    self.echo = n;
                    self.stall_until = Instant::after(Duration::from_millis(stall_ms));
                    if n > 0 {
                        // `end` follows the last echoed byte.
                        return;
                    }
                }
                _ => self.send("error: echo takes a byte count (at most 64 MiB) and a stall (at most 60,000 ms)"),
            },
            "bootctl" => match (arg, c.boot.ctl) {
                ("", Some(ctl)) => {
                    for s in 0..2u8 {
                        let slot = &ctl.slot[s as usize];
                        self.send(&alloc::format!(
                            "slot {} {} {} {}",
                            fbootctl::slot_name(s),
                            slot.state.name(),
                            slot.tries_left,
                            slot.version()
                        ));
                    }
                    self.send(&alloc::format!("active {}", fbootctl::slot_name(ctl.active)));
                    self.send(&alloc::format!("booted {}", fbootctl::slot_name(c.boot.slot)));
                }
                ("trial b", Some(_)) => {
                    let version = c.image.version.clone();
                    match c.boot.start_trial(1, &version) {
                        Ok(()) => self.send("ok"),
                        Err(e) => self.send(&alloc::format!("error: {}", e.describe())),
                    }
                }
                (_, None) => self.send("error: no A/B record"),
                _ => self.send("error: bootctl, or bootctl trial b"),
            },
            "stage" => {
                for line in self.stage.run(c, arg) {
                    self.send(&line);
                }
            }
            "terminal" => {
                for line in self.terminal.run(c, arg) {
                    self.send(&line);
                }
            }
            "monitor" => {
                for line in self.monitor.run(c, arg) {
                    self.send(&line);
                }
            }
            "cdpd" => {
                let line = self.cdpd(c, arg);
                self.send(&line);
            }
            "services" => {
                let lines: Vec<String> =
                    c.services.iter().map(|s| alloc::format!("svc {} {:?}", s.m.name, s.state)).collect();
                for line in lines {
                    self.send(&line);
                }
            }
            _ => self.send("error: unknown command"),
        }
        self.send("end");
    }
}

/// `echo`'s arguments: a byte count, and a stall in milliseconds (0 if none).
fn echo_args(arg: &str) -> Option<(usize, u64)> {
    let mut words = arg.split_whitespace();
    let n = words.next()?.parse().ok()?;
    let stall = match words.next() {
        Some(w) => w.parse().ok()?,
        None => 0,
    };
    words.next().is_none().then_some((n, stall))
}
