// SPDX-License-Identifier: Apache-2.0
//! The test door (e2e flavour only, compiled out of every other): line
//! commands on COM3 (0x3E8) for the CDP specs until cdpd exists (Phase 3).
//!
//! - `kill <service>`: ends the service's process; when it registers again,
//!   the door says `back <service> <ms>`.
//! - `log <n>`: the logger's newest `n` records, one `rec <svc> <msg>` each.
//! - `services`: one `svc <name> <state>` per service.
//!
//! Each answer ends with `end`.

use crate::services::Conductor;
use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::{resource, well_known};
use fstd::time::{Duration, Instant};
use fwire::proto::logger as l;

const COM3: u16 = 0x3E8;

pub struct Door {
    line: Vec<u8>,
    open: bool,
}

impl Door {
    pub fn open() -> Door {
        let ok = fstd::sys::resource_create(well_known::ROOT_RESOURCE, resource::IO_PORT, COM3 as u64, 8)
            .and_then(|r| fstd::sys::ioport_grant(r, COM3, 8))
            .is_ok();
        Door { line: Vec::new(), open: ok }
    }

    fn send(&self, s: &str) {
        for b in s.bytes().chain(core::iter::once(b'\n')) {
            let start = Instant::now();
            while fstd::sys::inb(COM3 + 5) & 0x20 == 0 && start.elapsed() < Duration::from_millis(50) {}
            fstd::sys::outb(COM3, b);
        }
    }

    /// Reads what has arrived; runs each whole line.
    pub fn poll(&mut self, c: &mut Conductor) {
        if !self.open {
            return;
        }
        for out in core::mem::take(&mut c.door_out) {
            self.send(&out);
            self.send("end");
        }
        while fstd::sys::inb(COM3 + 5) & 1 != 0 {
            let b = fstd::sys::inb(COM3);
            if b == b'\n' || b == b'\r' {
                if !self.line.is_empty() {
                    let cmd = String::from_utf8_lossy(&core::mem::take(&mut self.line)).into_owned();
                    self.run(c, cmd.trim());
                }
            } else if self.line.len() < 256 {
                self.line.push(b);
            }
        }
    }

    fn run(&mut self, c: &mut Conductor, cmd: &str) {
        let (verb, arg) = cmd.split_once(' ').unwrap_or((cmd, ""));
        match verb {
            "kill" => match c.services.iter_mut().find(|s| s.m.name == arg) {
                Some(s) if s.child.is_some() => {
                    s.killed_at = Some(Instant::now().as_ns());
                    let ok = s.child.as_ref().is_some_and(|ch| fstd::sys::process_kill(ch.process.raw()).is_ok());
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
