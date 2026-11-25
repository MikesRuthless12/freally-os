// SPDX-License-Identifier: Apache-2.0
//! cdpd's port on the network (TASK-184): CDP over WebSocket, as DevTools
//! speaks it, on port 9222, one runner at a time. Everything asked of
//! netstack is a request sent now and a reply read when it comes, in cdpd's
//! one wait, so conductor's door is served meanwhile. netstack listens for
//! cdpd only in e2e images, and cdpd is only in those.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::FErr;
use fjson::Value;
use fstd::ipc::Channel;
use fstd::time::{Duration, Instant};
use fwire::proto::net as nt;
use svc_cdpd::ws::{self, Event};

/// DevTools' own port.
pub const PORT: u16 = 9222;
/// How long an ACCEPT or a RECV waits before it's asked again.
const WAIT_MS: u32 = 60_000;
/// Tries at listening, 2 s apart: an image with no network card has no
/// netstack, and then CDP is COM3's alone.
const TRIES: u32 = 15;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Asked {
    Listen,
    Accept,
    /// A RECV, and a SEND of so many bytes, on connection `id`: a reply for
    /// a connection that has ended isn't the next one's.
    Recv(u32),
    Send(u32, usize),
    Close,
}

struct Conn {
    id: u32,
    /// The opening handshake's bytes so far; `None` once it's done.
    opening: Option<Vec<u8>>,
    reader: ws::Reader,
    /// Bytes for the runner, and whether a SEND is out.
    out: VecDeque<u8>,
    sending: bool,
    /// Ending: closed once what's to send has gone.
    closing: bool,
}

pub struct Port {
    netstack: Option<Channel>,
    txid: u32,
    /// What each request out is, by its txid.
    asked: Vec<(u32, Asked)>,
    conn: Option<Conn>,
    tries: u32,
    retry_at: u64,
}

impl Port {
    pub fn new() -> Port {
        Port { netstack: None, txid: 0, asked: Vec::new(), conn: None, tries: 0, retry_at: 0 }
    }

    /// The channel to wait on, once there is one.
    pub fn channel(&self) -> Option<u32> {
        self.netstack.as_ref().map(|c| c.0.raw())
    }

    /// When the wait should end for the next try at listening (else never).
    pub fn deadline(&self) -> u64 {
        if self.netstack.is_none() && self.tries < TRIES { self.retry_at } else { u64::MAX }
    }

    /// Listening, once netstack is up: tried every 2 s, `TRIES` times.
    pub fn tick(&mut self) {
        let now = Instant::now().as_ns();
        if self.netstack.is_some() || self.tries >= TRIES || now < self.retry_at {
            return;
        }
        self.tries += 1;
        self.retry_at = now + 2_000_000_000;
        self.netstack = fstd::service::connect_within("netstack", Duration::from_millis(500)).ok();
        if self.netstack.is_some() {
            self.ask(nt::TCP_LISTEN, &nt::Listen { port: PORT, backlog: 1 }, Asked::Listen);
        } else if self.tries == TRIES {
            fstd::info!("cdpd: no netstack: CDP over COM3 only");
        }
    }

    fn ask<Q: fwire::Wire>(&mut self, ordinal: u32, req: &Q, kind: Asked) {
        let Some(ch) = &self.netstack else { return };
        self.txid = self.txid.wrapping_add(1).max(1);
        match fwire::request(self.txid, ordinal, req) {
            Ok(m) if ch.send(&m, Vec::new()).is_ok() => self.asked.push((self.txid, kind)),
            _ => self.gone(),
        }
    }

    /// netstack went: everything starts again, a try at a time.
    fn gone(&mut self) {
        self.netstack = None;
        self.asked.clear();
        self.conn = None;
        self.tries = 0;
    }

    fn accept(&mut self) {
        self.ask(nt::TCP_ACCEPT, &nt::Accept { port: PORT, timeout_ms: WAIT_MS }, Asked::Accept);
    }

    fn recv(&mut self, id: u32) {
        self.ask(nt::TCP_RECV, &nt::Recv { id, max: 60 * 1024, timeout_ms: WAIT_MS }, Asked::Recv(id));
    }

    /// Each reply netstack has sent, taken; `call` answers a CDP method.
    pub fn readable(&mut self, call: &mut dyn FnMut(&str, &Value) -> Result<Value, String>) {
        loop {
            let Some(ch) = &self.netstack else { return };
            let m = match ch.try_recv() {
                Ok(m) => m,
                Err(FErr::ShouldWait) => return,
                Err(_) => return self.gone(),
            };
            let Ok((h, p)) = fwire::parse(&m.bytes) else { continue };
            let Some(k) = self.asked.iter().position(|(t, _)| *t == h.txid) else { continue };
            let (_, kind) = self.asked.remove(k);
            let ok = !h.is_error();
            let current = |id: u32| self.conn.as_ref().is_some_and(|c| c.id == id);
            match kind {
                Asked::Recv(id) | Asked::Send(id, _) if !current(id) => {}
                Asked::Listen if ok => {
                    fstd::info!("cdpd: CDP over WebSocket on port {PORT}");
                    self.accept();
                }
                Asked::Listen => {
                    let why = fwire::error_of(p).map(|(_, t)| t).unwrap_or_default();
                    fstd::warn!("cdpd: port {PORT} refused ({why}): CDP over COM3 only");
                    self.tries = TRIES;
                    self.netstack = None;
                }
                Asked::Accept => match fwire::from_payload::<nt::Accepted>(p) {
                    Ok(a) if ok => {
                        self.conn = Some(Conn {
                            id: a.id,
                            opening: Some(Vec::new()),
                            reader: ws::Reader::default(),
                            out: VecDeque::new(),
                            sending: false,
                            closing: false,
                        });
                        self.recv(a.id);
                    }
                    // None came in a minute: asked again. Any other refusal:
                    // listening starts over, a try at a time (never a spin).
                    _ if fwire::error_of(p).is_ok_and(|(code, _)| code == FErr::TimedOut as i32) => self.accept(),
                    _ => self.gone(),
                },
                Asked::Recv(_) => match fwire::from_payload::<nt::Received>(p) {
                    Ok(r) if ok => self.received(r, call),
                    _ => self.end(),
                },
                Asked::Send(_, n) => match &mut self.conn {
                    Some(c) if ok => {
                        c.out.drain(..n);
                        c.sending = false;
                    }
                    _ => self.end(),
                },
                Asked::Close => {}
            }
            self.flush();
        }
    }

    /// The runner's bytes: the opening handshake first, then its messages,
    /// each answered in turn.
    fn received(&mut self, r: nt::Received, call: &mut dyn FnMut(&str, &Value) -> Result<Value, String>) {
        let Some(c) = &mut self.conn else { return };
        match &mut c.opening {
            Some(open) => {
                open.extend_from_slice(&r.data);
                match ws::handshake(open) {
                    Ok(None) => {}
                    Ok(Some((_, response, used))) => {
                        let rest = open.split_off(used);
                        c.out.extend(response);
                        c.reader.feed(&rest);
                        c.opening = None;
                    }
                    Err(e) => {
                        fstd::warn!("cdpd: a connection to port {PORT} that wasn't WebSocket: {e:?}");
                        c.out.extend(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");
                        c.closing = true;
                    }
                }
            }
            None => c.reader.feed(&r.data),
        }
        while c.opening.is_none() && !c.closing {
            match c.reader.event() {
                Ok(None) => break,
                Ok(Some(Event::Text(t))) => {
                    let reply = svc_cdpd::message::answer(&t, call);
                    c.out.extend(ws::frame(ws::op::TEXT, reply.as_bytes()));
                }
                Ok(Some(Event::Ping(p))) => c.out.extend(ws::frame(ws::op::PONG, &p)),
                Ok(Some(Event::Pong | Event::Binary(_))) => {}
                Ok(Some(Event::Close(code))) => {
                    let body = code.map(u16::to_be_bytes);
                    c.out.extend(ws::frame(ws::op::CLOSE, body.as_ref().map_or(&[][..], |b| &b[..])));
                    c.closing = true;
                }
                Err(e) => {
                    // § 7.4.1: 1009 too big, 1002 a protocol error.
                    let code: u16 = if e == ws::Error::TooBig { 1009 } else { 1002 };
                    c.out.extend(ws::frame(ws::op::CLOSE, &code.to_be_bytes()));
                    c.closing = true;
                }
            }
        }
        if r.end {
            c.closing = true;
        }
        if !c.closing {
            let id = c.id;
            self.recv(id);
        }
    }

    /// The connection ends now, whatever is left to send.
    fn end(&mut self) {
        if let Some(c) = self.conn.take() {
            self.ask(nt::TCP_CLOSE, &nt::Socket { id: c.id }, Asked::Close);
            self.accept();
        }
    }

    /// What's to send goes, a SEND at a time; a connection ending closes once it has.
    fn flush(&mut self) {
        let Some(c) = &mut self.conn else { return };
        if c.sending {
            return;
        }
        if !c.out.is_empty() {
            let n = c.out.len().min(60 * 1024);
            let data: Vec<u8> = c.out.iter().take(n).copied().collect();
            let id = c.id;
            c.sending = true;
            self.ask(nt::TCP_SEND, &nt::Send { id, data }, Asked::Send(id, n));
        } else if c.closing {
            self.end();
        }
    }
}
