// SPDX-License-Identifier: Apache-2.0
//! The test door's TCP (e2e flavour only, TASK-177): connections through
//! netstack's net protocol, as any client makes them.
//! - `tcp echo <ip> <port> <bytes>` connects, sends `<bytes>` bytes of a
//!   pattern (32 KiB a call) while reading what comes back, checks every
//!   byte, closes, and waits for the peer's end: `echoed <n> bytes in <ms> ms`.
//! - `tcp connect <ip> <port> [ms]` connects (waiting up to `ms`, else 10 s)
//!   and closes: `connected`.
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
}

const USAGE: &str = "tcp echo <ip> <port> <bytes, at most 4 MiB> | tcp connect <ip> <port> [ms]";

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
            _ => Err(String::from(USAGE)),
        };
        r.unwrap_or_else(|e| {
            self.client = None;
            alloc::vec![alloc::format!("error: {e}")]
        })
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
