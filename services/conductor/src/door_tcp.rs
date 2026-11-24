// SPDX-License-Identifier: Apache-2.0
//! The test door's TCP (e2e flavour only, TASK-177): connections through
//! netstack's net protocol, as any client makes them.
//! - `tcp echo <ip> <port> <bytes>` connects, sends `<bytes>` bytes of a
//!   pattern (32 KiB a call) while reading what comes back, checks every
//!   byte, closes, and waits for the peer's end: `echoed <n> bytes in <ms> ms`.
//! - `tcp connect <ip> <port> [ms]` connects (waiting up to `ms`, else 10 s)
//!   and closes: `connected`.
//! - `tcp hold <ip> <port> <n>` opens up to `n` connections at once, then
//!   closes them all: `opened <k> of <n>`, and the first refusal's words
//!   (TASK-185: netstack's limit per client).
//!
//! - `net waiting ping <ip> <n>` sends `n` pings to `<ip>` (3 s each) at
//!   once, and `net waiting resolve <n>` `n` lookups of names the test
//!   network's DNS never answers; 2 s later: `waiting <k>, refused <r>,
//!   answered <a>`, and the first refusal's words (TASK-185: what one client
//!   may have waiting). The rest are answered later, and not read.
//!
//! Any failure: `error: …`, netstack's reason in it (`refused`, `no answer`).

use crate::services::Conductor;
use alloc::string::String;
use alloc::vec::Vec;
use fstd::ipc::Client;
use fstd::time::{Duration, Instant};
use fwire::proto::net as nt;

#[derive(Default)]
pub struct TcpDoor {
    client: Option<Client>,
    /// Requests `net waiting` has sent: where its next txids start.
    asked: u32,
}

const USAGE: &str =
    "tcp echo <ip> <port> <bytes, at most 4 MiB> | tcp connect <ip> <port> [ms] | tcp hold <ip> <port> <n, at most 64>";

impl TcpDoor {
    pub fn run(&mut self, c: &mut Conductor, arg: &str) -> Vec<String> {
        if self.client.is_none() {
            self.client = c.open_connection("netstack", "conductor").map(Client::new);
        }
        let words: Vec<&str> = arg.split_whitespace().collect();
        let r = match words.as_slice() {
            ["echo", ip, port, n] => match (crate::door_netdev::ipv4(ip), port.parse::<u16>(), n.parse::<usize>()) {
                (Some(to), Ok(port), Ok(n)) if n <= 4 << 20 => self.echo(to, port, n),
                _ => Err(String::from(USAGE)),
            },
            ["connect", ip, port, rest @ ..] => {
                let ms = match rest {
                    [] => Some(10_000),
                    [m] => m.parse::<u32>().ok().filter(|m| *m <= 60_000),
                    _ => None,
                };
                match (crate::door_netdev::ipv4(ip), port.parse::<u16>(), ms) {
                    (Some(to), Ok(port), Some(ms)) => self.connect(to, port, ms).and_then(|id| {
                        self.call::<_, ()>(nt::TCP_CLOSE, &nt::Socket { id }, 2)?;
                        Ok(alloc::vec![String::from("connected")])
                    }),
                    _ => Err(String::from(USAGE)),
                }
            }
            ["hold", ip, port, n] => match (crate::door_netdev::ipv4(ip), port.parse::<u16>(), n.parse::<usize>()) {
                (Some(to), Ok(port), Ok(n)) if n <= 64 => {
                    let mut held = Vec::new();
                    let mut why = String::new();
                    for _ in 0..n {
                        match self.connect(to, port, 5_000) {
                            Ok(id) => held.push(id),
                            Err(e) => {
                                why = e;
                                break;
                            }
                        }
                    }
                    for id in &held {
                        let _ = self.call::<_, ()>(nt::TCP_CLOSE, &nt::Socket { id: *id }, 2);
                    }
                    Ok(alloc::vec![alloc::format!("opened {} of {n}", held.len()), why])
                }
                _ => Err(String::from(USAGE)),
            },
            _ => Err(String::from(USAGE)),
        };
        r.unwrap_or_else(|e| {
            self.client = None;
            alloc::vec![alloc::format!("error: {e}")]
        })
    }

    /// `net waiting …`: many requests sent before any answer is read.
    pub fn waiting(&mut self, c: &mut Conductor, arg: &str) -> Vec<String> {
        const USAGE: &str = "net waiting ping <ip> <n, at most 64> | net waiting resolve <n, at most 64>";
        if self.client.is_none() {
            self.client = c.open_connection("netstack", "conductor").map(Client::new);
        }
        let words: Vec<&str> = arg.split_whitespace().collect();
        // Txids of their own, above any the client's calls reach, and new
        // each time: an earlier time's late answers aren't this one's.
        let first = 0x4000_0000 + self.asked;
        let txid = |k: u16| first + u32::from(k);
        let asks: Option<Vec<_>> = match words.as_slice() {
            ["waiting", "ping", ip, n] => match (crate::door_netdev::ipv4(ip), n.parse::<u16>()) {
                (Some(to), Ok(n)) if n <= 64 => (0..n)
                    .map(|k| fwire::request(txid(k), nt::PING, &nt::Ping { to, seq: k, timeout_ms: 3_000 }))
                    .collect::<Result<_, _>>()
                    .ok(),
                _ => None,
            },
            ["waiting", "resolve", n] => match n.parse::<u16>() {
                Ok(n) if n <= 64 => (0..n)
                    .map(|k| {
                        let q = nt::Resolve { name: alloc::format!("w{k}.silent.freally.local"), kind: 1 };
                        fwire::request(txid(k), nt::RESOLVE, &q)
                    })
                    .collect::<Result<_, _>>()
                    .ok(),
                _ => None,
            },
            _ => None,
        };
        let Some(asks) = asks else { return alloc::vec![String::from(USAGE)] };
        let Some(cl) = self.client.as_ref() else { return alloc::vec![String::from("error: netstack isn't up")] };
        let n = asks.len();
        self.asked = self.asked.wrapping_add(n as u32) % 0x1000_0000;
        for m in &asks {
            if let Err(e) = cl.channel.send(m, Vec::new()) {
                self.client = None;
                return alloc::vec![alloc::format!("error: {e:?}")];
            }
        }
        // What netstack did with each in 2 s: a refusal comes at once; a ping
        // to nobody waits its 3 s, a lookup nobody answers netstack's 10.
        let (mut refused, mut answered, mut why) = (0, 0, String::new());
        let deadline = Instant::after(Duration::from_secs(2));
        while refused + answered < n {
            let Ok(m) = cl.channel.recv(deadline) else { break };
            let Ok((h, p)) = fwire::parse(&m.bytes) else { continue };
            if !h.is_reply() || h.txid < txid(0) || h.txid >= txid(n as u16) {
                continue;
            }
            match h.is_error().then(|| fwire::error_of(p)) {
                Some(Ok((code, text))) if code == fcore_abi::FErr::Busy as i32 => {
                    refused += 1;
                    if why.is_empty() {
                        why = text;
                    }
                }
                _ => answered += 1,
            }
        }
        alloc::vec![alloc::format!("waiting {}, refused {refused}, answered {answered}", n - refused - answered), why]
    }

    fn call<Q: fwire::Wire, R: fwire::Wire>(&mut self, ordinal: u32, req: &Q, secs: u64) -> Result<R, String> {
        let cl = self.client.as_mut().ok_or("netstack isn't up")?;
        cl.call::<_, R>(ordinal, req, Vec::new(), Instant::after(Duration::from_secs(secs)))
            .map(|(r, _)| r)
            .map_err(|e| alloc::format!("{e:?}"))
    }

    fn connect(&mut self, to: [u8; 4], port: u16, timeout_ms: u32) -> Result<u32, String> {
        let c = nt::Connect { to, port, timeout_ms };
        self.call::<_, nt::Socket>(nt::TCP_CONNECT, &c, timeout_ms as u64 / 1000 + 2).map(|s| s.id)
    }

    fn echo(&mut self, to: [u8; 4], port: u16, n: usize) -> Result<Vec<String>, String> {
        let start = Instant::now();
        let id = self.connect(to, port, 10_000)?;
        let data: Vec<u8> = (0..n).map(|i| (i * 31 + i / 997) as u8).collect();
        let (mut sent, mut got) = (0, Vec::with_capacity(n));
        while got.len() < n {
            if sent < n {
                let end = (sent + 32 * 1024).min(n);
                self.call::<_, nt::Sent>(nt::TCP_SEND, &nt::Send { id, data: data[sent..end].to_vec() }, 10)?;
                sent = end;
            }
            // While sending, take what's there; once all is sent, wait for the rest.
            let wait = if sent < n { 0 } else { 5_000 };
            let r =
                self.call::<_, nt::Received>(nt::TCP_RECV, &nt::Recv { id, max: 60 * 1024, timeout_ms: wait }, 10)?;
            if r.end {
                return Err(alloc::format!("the peer closed after {} of {n} bytes", got.len()));
            }
            if r.data.is_empty() && sent == n {
                return Err(alloc::format!("nothing more in 5 s after {} of {n} bytes", got.len()));
            }
            got.extend(r.data);
        }
        if let Some(i) = got.iter().zip(&data).position(|(a, b)| a != b) {
            return Err(alloc::format!("byte {i} came back changed"));
        }
        self.call::<_, ()>(nt::TCP_CLOSE, &nt::Socket { id }, 2)?;
        // The echo ends when its input does: its FIN comes back.
        let r = self.call::<_, nt::Received>(nt::TCP_RECV, &nt::Recv { id, max: 1, timeout_ms: 5_000 }, 7)?;
        if !r.end || got.len() != n {
            return Err(String::from("no end from the peer after the close"));
        }
        let ms = start.elapsed().as_millis();
        Ok(alloc::vec![alloc::format!("echoed {n} bytes in {ms} ms")])
    }
}
