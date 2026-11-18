// SPDX-License-Identifier: Apache-2.0
//! logger (TASK-039): keeps the system's log in a 4 MiB ring.
//!
//! - Every service writes to it (`write`, one-way); `read` pages through it.
//! - It drains the kernel's early ring (`system_info(KERNEL_LOG)`) so the
//!   kernel's lines are in the same log.
//! - In dev and e2e images conductor hands it COM1, and it mirrors every
//!   service's line there as `[service] line` (the kernel's own lines are on
//!   COM1 already).
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

mod ring;

use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::{FErr, signal, topic};
use fstd::handle::Handle;
use fstd::ipc::Channel;
use fstd::time::{Duration, Instant};
use fwire::proto::conductor as c;
use fwire::proto::logger as l;
use ring::Ring;

fstd::program!(run);

const COM1: u16 = 0x3F8;
const BUDGET: usize = 4 * 1024 * 1024;

struct Logger {
    ring: Ring,
    mirror: Option<Handle>,
    kernel_seen: u64,
    kernel_partial: Vec<u8>,
}

impl Logger {
    fn mirror_line(&self, svc: &str, msg: &str) {
        if self.mirror.is_none() {
            return;
        }
        for b in "[".bytes().chain(svc.bytes()).chain("] ".bytes()).chain(msg.bytes()).chain("\r\n".bytes()) {
            let start = Instant::now();
            while fstd::sys::inb(COM1 + 5) & 0x20 == 0 && start.elapsed() < Duration::from_millis(10) {}
            fstd::sys::outb(COM1, b);
        }
    }

    fn write(&mut self, svc: &str, level: u8, msg: &str) {
        self.ring.push(Instant::now().as_ns(), level, svc, msg);
        self.mirror_line(svc, msg);
    }

    /// The kernel's lines since the last drain, as records from `kernel`.
    fn drain_kernel(&mut self) {
        let mut buf = alloc::vec![0u8; 8 + 64 * 1024];
        let Ok(n) = fstd::sys::system_info(topic::KERNEL_LOG, &mut buf) else { return };
        let n = n.min(buf.len());
        if n < 8 {
            return;
        }
        let total = u64::from_le_bytes(buf[..8].try_into().unwrap_or_default());
        let held = &buf[8..n];
        let new = (total.saturating_sub(self.kernel_seen) as usize).min(held.len());
        self.kernel_seen = total;
        self.kernel_partial.extend_from_slice(&held[held.len() - new..]);
        while let Some(i) = self.kernel_partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.kernel_partial.drain(..=i).collect();
            let text = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
            let level = if text.contains("ERROR") {
                l::ERROR
            } else if text.contains("WARNING") {
                l::WARN
            } else {
                l::INFO
            };
            self.ring.push(Instant::now().as_ns(), level, "kernel", text.trim_end());
        }
    }
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let mirror = fstd::service::take_handle("serial").filter(|h| fstd::sys::ioport_grant(h.raw(), COM1, 8).is_ok());
    let mut lg = Logger { ring: Ring::new(BUDGET), mirror, kernel_seen: 0, kernel_partial: Vec::new() };
    lg.drain_kernel();
    let Ok((listen, give)) = Channel::pair() else { return 1 };
    if fstd::service::register(give).is_err() {
        return 1;
    }
    let mirrored = if lg.mirror.is_some() { "COM1" } else { "nowhere" };
    lg.write("logger", l::INFO, &alloc::format!("logger: ready (mirroring to {mirrored})"));
    let mut clients: Vec<(String, Channel)> = Vec::new();
    loop {
        let mut items = alloc::vec![[listen.0.raw(), signal::READABLE]];
        items.extend(clients.iter().map(|(_, ch)| [ch.0.raw(), signal::READABLE | signal::PEER_CLOSED]));
        match fstd::sys::wait_many(&items, Instant::after(Duration::from_millis(100))) {
            Ok(0) => {
                if let Ok(m) = listen.recv(0)
                    && let Ok((h, p)) = fwire::parse(&m.bytes)
                    && h.ordinal == c::INCOMING
                    && let Ok(inc) = fwire::from_payload::<c::Incoming>(p)
                    && let Some(ch) = m.handles.into_iter().next()
                {
                    clients.push((inc.client, Channel(ch)));
                }
            }
            Ok(k) => {
                let i = k - 1;
                if !serve(&mut lg, &clients[i].0, &clients[i].1) {
                    clients.remove(i);
                }
            }
            Err(FErr::TimedOut) => {}
            Err(_) => fstd::time::sleep(Duration::from_millis(10)),
        }
        lg.drain_kernel();
    }
}

/// Serves every message waiting from one client; false once it's gone.
fn serve(lg: &mut Logger, who: &str, ch: &Channel) -> bool {
    loop {
        let mut bytes = alloc::vec![0u8; 4096];
        let mut hs = [0u32; 4];
        match fstd::sys::channel_read(ch.0.raw(), &mut bytes, &mut hs) {
            Ok((n, _)) if fstd::inventory::seen(ch, &bytes[..n]) => {}
            Ok((n, _)) => {
                let Ok((h, p)) = fwire::parse(&bytes[..n]) else { continue };
                match h.ordinal {
                    l::WRITE => {
                        fstd::inventory::count(l::WRITE);
                        if let Ok(w) = fwire::from_payload::<l::Write>(p) {
                            lg.write(who, w.level, &w.msg);
                        }
                    }
                    l::READ => {
                        let reply = match fwire::from_payload::<l::Read>(p) {
                            Ok(r) => fwire::reply(h.txid, h.ordinal, &lg.ring.read(r.cursor, r.limit, &r.filter)),
                            Err(e) => fwire::error_reply(h.txid, h.ordinal, FErr::InvalidArgs as i32, e.describe()),
                        };
                        if let Ok(m) = reply {
                            let _ = ch.send(&m, Vec::new());
                        }
                    }
                    _ => {
                        if let Ok(m) = fwire::error_reply(h.txid, h.ordinal, FErr::NotSupported as i32, "not served") {
                            let _ = ch.send(&m, Vec::new());
                        }
                    }
                }
            }
            Err(FErr::ShouldWait) => return true,
            Err(FErr::BufferTooSmall) => return true,
            Err(_) => return false,
        }
    }
}
