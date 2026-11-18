// SPDX-License-Identifier: Apache-2.0
//! drv-uart16550 (TASK-044, FR-021; dev and e2e images only): COM2, the
//! Brain's line, and COM3, cdpd's (and the test door's until cdpd exists).
//! COM1 is the logger's own mirror: it has to work from the first line, before
//! devmgr or any driver runs.
//!
//! - Each port: 115,200 8N1, FIFOs on, interrupt driven (IRQ 3 and 4).
//! - `open(port)` gives one client a channel for the port: `data` events carry
//!   what arrived, `write` messages what to send.
//! - Nothing is lost: when a client falls behind (its channel is `Busy`), the
//!   driver stops reading the port, the UART's FIFO fills, and the far end is
//!   held back (QEMU stops reading its socket; a real line drops nothing
//!   until the FIFO overruns, which the line status reports).

#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::{FErr, signal, syscall as nr};
use fstd::handle::Handle;
use fstd::ipc::{Channel, Client};
use fstd::time::{Duration, Instant};
use fwire::HandleRef;
use fwire::proto::conductor as c;
use fwire::proto::devmgr as d;
use fwire::proto::serial as s;

fstd::program!(run);

/// Received bytes go to the client in messages of at most this many.
const CHUNK: usize = 60_000;
/// Bytes waiting to be sent before the driver stops taking more from the client.
const TX_CAP: usize = 1 << 20;

struct Port {
    number: u8,
    base: u16,
    /// Its interrupt line (an index into the bound lines).
    line: usize,
    client: Option<Channel>,
    /// Arrived, not yet given to the client.
    rx: Vec<u8>,
    /// To send.
    tx: VecDeque<u8>,
    /// The client is behind: the port isn't read until it catches up.
    held: bool,
    ier: u8,
}

impl Port {
    fn reg(&self, r: u16) -> u16 {
        self.base + r
    }

    /// 115,200 8N1, FIFOs on (trigger at 14), OUT2 (the PC's IRQ gate) on.
    /// False if no UART answers here. Turning the FIFO on clears it, so a
    /// byte already waiting is kept first: nothing sent before the driver
    /// started is lost.
    fn init(&mut self) -> bool {
        fstd::sys::outb(self.reg(7), 0x5A);
        if fstd::sys::inb(self.reg(7)) != 0x5A {
            return false;
        }
        fstd::sys::outb(self.reg(1), 0);
        for _ in 0..16 {
            if fstd::sys::inb(self.reg(5)) & 0x01 == 0 {
                break;
            }
            self.rx.push(fstd::sys::inb(self.reg(0)));
        }
        fstd::sys::outb(self.reg(3), 0x80);
        fstd::sys::outb(self.reg(0), 1);
        fstd::sys::outb(self.reg(1), 0);
        fstd::sys::outb(self.reg(3), 0x03);
        fstd::sys::outb(self.reg(2), 0xC7);
        fstd::sys::outb(self.reg(4), 0x0B);
        self.update_ier();
        true
    }

    fn set_ier(&mut self, ier: u8) {
        if ier != self.ier {
            self.ier = ier;
            fstd::sys::outb(self.reg(1), ier);
        }
    }

    /// The ISA line is edge-triggered: a UART whose interrupt was still
    /// pending when the line was acked holds it high and never makes another
    /// edge. Interrupts off and on again: a fresh edge if anything is pending.
    fn kick(&self) {
        fstd::sys::outb(self.reg(1), 0);
        fstd::sys::outb(self.reg(1), self.ier);
    }

    /// Reads what the receive FIFO holds now (unless held). No more: bytes
    /// read as they trickle in cost a round trip each (QEMU tops the FIFO up
    /// a byte at a time once it's past the trigger level), where the next
    /// interrupt brings 14.
    fn receive(&mut self) {
        if !self.held && self.rx.len() < CHUNK * 4 {
            let mut n = 0;
            // "Received data available": at least the trigger level (14) is
            // waiting, so those need no line-status check each.
            if fstd::sys::inb(self.reg(2)) & 0x0F == 0x04 {
                for _ in 0..14 {
                    self.rx.push(fstd::sys::inb(self.reg(0)));
                }
                n = 14;
            }
            while n < 16 && fstd::sys::inb(self.reg(5)) & 0x01 != 0 {
                self.rx.push(fstd::sys::inb(self.reg(0)));
                n += 1;
            }
        }
    }

    /// Fills the send FIFO while it says it's empty (THRE: all 16 bytes free),
    /// up to four times: a real line drains 16 bytes per 1.4 ms, so that's one
    /// fill per interrupt; QEMU's sends at once, so it's four.
    fn send(&mut self) {
        for _ in 0..4 {
            if self.tx.is_empty() || fstd::sys::inb(self.reg(5)) & 0x20 == 0 {
                break;
            }
            for _ in 0..16 {
                match self.tx.pop_front() {
                    Some(b) => fstd::sys::outb(self.reg(0), b),
                    None => break,
                }
            }
        }
    }

    /// Receive interrupts while a client has the port and isn't behind (a
    /// port no one has opened isn't read: what arrives waits in the UART, and
    /// the sender is held back); send interrupts while there's more to send.
    fn update_ier(&mut self) {
        let reading = self.client.is_some() && !self.held && self.rx.len() < CHUNK * 4;
        let rx_on = if reading { 0x01 } else { 0 };
        let tx_on = if self.tx.is_empty() { 0 } else { 0x02 };
        self.set_ier(rx_on | tx_on);
    }

    /// Gives the client what has arrived. A client that's behind holds the port.
    fn deliver(&mut self) {
        // No client: what arrived waits for the next one.
        let Some(ch) = &self.client else { return };
        while !self.rx.is_empty() {
            let n = self.rx.len().min(CHUNK);
            let Ok(m) = fwire::event(s::DATA, &s::Bytes { data: self.rx[..n].to_vec() }) else { return };
            match ch.send(&m, Vec::new()) {
                Ok(()) => {
                    self.rx.drain(..n);
                }
                Err(FErr::Busy) => {
                    self.held = true;
                    return;
                }
                Err(_) => {
                    // The client is gone: the port is free for the next one,
                    // which gets what it didn't.
                    self.client = None;
                    self.tx.clear();
                    return;
                }
            }
        }
        self.held = false;
    }

    /// Takes what the client wants sent.
    fn take_writes(&mut self) {
        let Some(ch) = &self.client else { return };
        while self.tx.len() < TX_CAP {
            match ch.try_recv() {
                Ok(m) => {
                    if let Ok((h, p)) = fwire::parse(&m.bytes)
                        && h.ordinal == s::WRITE
                        && let Ok(b) = fwire::from_payload::<s::Bytes>(p)
                    {
                        fstd::inventory::count(s::WRITE);
                        self.tx.extend(b.data);
                    }
                }
                Err(FErr::ShouldWait) => break,
                Err(_) => {
                    self.client = None;
                    self.tx.clear();
                    break;
                }
            }
        }
    }
}

struct Lines {
    irqs: Vec<Handle>,
    gsis: Vec<(u64, u64)>,
}

fn resources() -> Result<Lines, String> {
    let devmgr = fstd::service::connect("devmgr").map_err(|e| alloc::format!("devmgr: {e:?}"))?;
    let deadline = Instant::after(Duration::from_secs(2));
    let (r, handles) = Client::new(devmgr)
        .call::<_, d::Resources>(d::RESOURCES, &(), Vec::new(), deadline)
        .map_err(|e| alloc::format!("devmgr's resources: {e:?}"))?;
    let mut by_name: Vec<(String, Handle)> = r.handles.iter().map(|(n, _)| n.clone()).zip(handles).collect();
    let mut take = |name: &str| {
        by_name
            .iter()
            .position(|(n, _)| n == name)
            .map(|i| by_name.swap_remove(i).1)
            .ok_or_else(|| alloc::format!("devmgr gave no {name}"))
    };
    let v = |k: &str| r.values.iter().find(|(n, _)| n == k).map_or(0, |(_, x)| *x);
    let mut lines = Lines { irqs: Vec::new(), gsis: Vec::new() };
    for (name, base) in [("com2", 0x2F8u16), ("com3", 0x3E8)] {
        let ports = take(name)?;
        fstd::sys::ioport_grant(ports.raw(), base, 8).map_err(|e| alloc::format!("{name}: {}", e.describe()))?;
        lines.irqs.push(take(&alloc::format!("{name}-irq"))?);
        lines.gsis.push((v(&alloc::format!("{name}-gsi")), v(&alloc::format!("{name}-flags"))));
    }
    Ok(lines)
}

fn bind(res: &Handle, (gsi, flags): (u64, u64)) -> Result<Handle, FErr> {
    let h = fstd::sys::call1(nr::INTERRUPT_BIND, [res.raw() as u64, gsi, flags, 0, 0, 0])?;
    Handle::from_raw(h as u32).ok_or(FErr::Internal)
}

/// One request on a connection: `open`. False when the connection is gone.
fn serve(conn: &Channel, ports: &mut [Port]) -> bool {
    loop {
        let m = match conn.try_recv() {
            Ok(m) => m,
            Err(FErr::ShouldWait) => return true,
            Err(_) => return false,
        };
        let Ok((h, p)) = fwire::parse(&m.bytes) else { continue };
        let (reply, handles) = match (h.ordinal, fwire::from_payload::<s::Open>(p)) {
            (s::OPEN, Ok(o)) => match ports.iter_mut().find(|x| x.number == o.port) {
                Some(port) if port.client.is_none() => match Channel::pair() {
                    Ok((mine, theirs)) => {
                        port.client = Some(mine);
                        port.held = false;
                        (fwire::reply(h.txid, h.ordinal, &s::Opened { channel: HandleRef(0) }), alloc::vec![theirs.0])
                    }
                    Err(e) => (fwire::error_reply(h.txid, h.ordinal, e as i32, "no channel"), Vec::new()),
                },
                Some(_) => (
                    fwire::error_reply(h.txid, h.ordinal, FErr::AlreadyExists as i32, "the port has a client"),
                    Vec::new(),
                ),
                None => (fwire::error_reply(h.txid, h.ordinal, FErr::NotFound as i32, "no such port"), Vec::new()),
            },
            _ => (fwire::error_reply(h.txid, h.ordinal, FErr::NotSupported as i32, "not served"), Vec::new()),
        };
        if let Ok(r) = reply {
            let _ = conn.send(&r, handles);
        }
    }
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    let lines = match resources() {
        Ok(l) => l,
        Err(e) => {
            fstd::error!("drv-uart16550: {e}");
            return 1;
        }
    };
    let mut ports: Vec<Port> = [(2u8, 0x2F8u16, 0usize), (3, 0x3E8, 1)]
        .into_iter()
        .map(|(number, base, line)| Port {
            number,
            base,
            line,
            client: None,
            rx: Vec::new(),
            tx: VecDeque::new(),
            held: false,
            ier: 0xFF,
        })
        .filter_map(|mut p| p.init().then_some(p))
        .collect();
    let mut irqs = Vec::new();
    for (res, gsi) in lines.irqs.iter().zip(&lines.gsis) {
        match bind(res, *gsi) {
            Ok(h) => irqs.push(h),
            Err(e) => {
                fstd::error!("drv-uart16550: can't bind GSI {}: {}", gsi.0, e.describe());
                return 1;
            }
        }
    }
    let names: Vec<String> = ports.iter().map(|p| alloc::format!("COM{}", p.number)).collect();
    fstd::info!("drv-uart16550: {} at 115200 8N1, interrupt driven", names.join(" and "));
    let Ok((listen, give)) = Channel::pair() else { return 1 };
    if fstd::service::register(give).is_err() {
        return 1;
    }
    let mut conns: Vec<Channel> = Vec::new();
    loop {
        // What wakes the driver: its lines, new connections, requests, and
        // each client's writes (while there's room to take them).
        let mut items: Vec<[u32; 2]> = irqs.iter().map(|h| [h.raw(), signal::SIGNALED]).collect();
        items.push([listen.0.raw(), signal::READABLE]);
        items.extend(conns.iter().map(|ch| [ch.0.raw(), signal::READABLE | signal::PEER_CLOSED]));
        let clients: Vec<usize> =
            (0..ports.len()).filter(|&i| ports[i].client.is_some() && ports[i].tx.len() < TX_CAP).collect();
        for &i in &clients {
            if let Some(ch) = &ports[i].client {
                items.push([ch.0.raw(), signal::READABLE | signal::PEER_CLOSED]);
            }
        }
        // A held port is retried soon: the client may have caught up.
        let deadline = if ports.iter().any(|p| p.held) { Instant::after(Duration::from_millis(2)) } else { u64::MAX };
        let first_conn = irqs.len() + 1;
        let first_client = first_conn + conns.len();
        match fstd::sys::wait_many(&items, deadline) {
            Ok(k) if k < irqs.len() => {
                for p in ports.iter_mut().filter(|p| p.line == k) {
                    p.receive();
                    p.send();
                    p.update_ier();
                }
                let _ = fstd::sys::call1(nr::INTERRUPT_ACK, [irqs[k].raw() as u64, 0, 0, 0, 0, 0]);
                for p in ports.iter().filter(|p| p.line == k) {
                    p.kick();
                }
            }
            Ok(k) if k == irqs.len() => {
                if let Ok(m) = listen.try_recv()
                    && let Ok((h, _)) = fwire::parse(&m.bytes)
                    && h.ordinal == c::INCOMING
                    && let Some(ch) = m.handles.into_iter().next()
                {
                    conns.push(Channel(ch));
                }
            }
            Ok(k) if k < first_client => {
                if !serve(&conns[k - first_conn], &mut ports) {
                    conns.remove(k - first_conn);
                }
            }
            Ok(k) => {
                if let Some(&i) = clients.get(k - first_client) {
                    ports[i].take_writes();
                }
            }
            Err(FErr::TimedOut) => {}
            Err(_) => fstd::time::sleep(Duration::from_millis(10)),
        }
        // Hand over what arrived; send what the client gave.
        for p in ports.iter_mut().filter(|p| p.client.is_some() || !p.rx.is_empty()) {
            p.deliver();
            p.send();
            p.update_ier();
        }
    }
}
