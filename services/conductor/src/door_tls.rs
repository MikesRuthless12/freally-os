// SPDX-License-Identifier: Apache-2.0
//! The test door's TLS (e2e flavour only, TASK-180): ftls over netstack's TCP,
//! as a client of the net protocol.
//! - `tls get <ip> <port> <host> <root DER in hex> <path>` connects, checks the
//!   server's chain for `<host>` against that root alone, sends
//!   `GET <path> HTTP/1.0`, reads to the server's close: `tls <suite> <host>`,
//!   then the response's first line, then `got <n> bytes`.
//!
//! Any failure: `error: …` (`certificate Name`, `alert …`, netstack's reason).

use crate::services::Conductor;
use alloc::string::String;
use alloc::vec::Vec;
use fstd::ipc::Client;
use fstd::time::{Duration, Instant};
use fwire::proto::net as nt;

#[derive(Default)]
pub struct TlsDoor {
    client: Option<Client>,
}

const USAGE: &str = "tls get <ip> <port> <host> <root DER in hex> <path>";

fn unhex(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if b.len() % 2 != 0 {
        return None;
    }
    b.chunks(2).map(|p| u8::from_str_radix(core::str::from_utf8(p).ok()?, 16).ok()).collect()
}

fn why(e: ftls::Error) -> String {
    match e {
        ftls::Error::Certificate(r) => alloc::format!("certificate {r:?}"),
        ftls::Error::Sent(a) => alloc::format!("sent {a:?}"),
        ftls::Error::Received(a) => alloc::format!("alert {a:?}"),
        other => alloc::format!("{other:?}"),
    }
}

impl TlsDoor {
    pub fn run(&mut self, c: &mut Conductor, arg: &str) -> Vec<String> {
        if self.client.is_none() {
            self.client = c.open_connection("netstack", "conductor").map(Client::new);
        }
        let words: Vec<&str> = arg.split_whitespace().collect();
        let r = match words.as_slice() {
            ["get", ip, port, host, root, path] => {
                match (crate::door_netdev::ipv4(ip), port.parse::<u16>(), unhex(root)) {
                    (Some(to), Ok(port), Some(root)) => self.get(to, port, host, &root, path),
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

    /// Sends what the client has for the server.
    fn flush(&mut self, id: u32, tls: &mut ftls::Client) -> Result<(), String> {
        let out = tls.take_output();
        for piece in out.chunks(60 * 1024) {
            self.call::<_, nt::Sent>(nt::TCP_SEND, &nt::Send { id, data: piece.to_vec() }, 10)?;
        }
        Ok(())
    }

    fn get(&mut self, to: [u8; 4], port: u16, host: &str, root: &[u8], path: &str) -> Result<Vec<String>, String> {
        let mut entropy = [0u8; 96];
        frandom::fill(&mut entropy).map_err(|_| String::from("no randomness"))?;
        let now = fstd::time::utc_ns() / 1_000_000_000;
        let roots = [root];
        let mut tls = ftls::Client::new(host, &roots, now, &entropy).map_err(why)?;
        let id = self.call::<_, nt::Socket>(nt::TCP_CONNECT, &nt::Connect { to, port, timeout_ms: 10_000 }, 12)?.id;
        let result = self.exchange(id, &mut tls, host, path);
        let _ = self.call::<_, ()>(nt::TCP_CLOSE, &nt::Socket { id }, 2);
        result
    }

    fn exchange(&mut self, id: u32, tls: &mut ftls::Client, host: &str, path: &str) -> Result<Vec<String>, String> {
        self.flush(id, tls)?;
        let recv = nt::Recv { id, max: 60 * 1024, timeout_ms: 5_000 };
        while !tls.is_connected() {
            let r = self.call::<_, nt::Received>(nt::TCP_RECV, &recv, 7)?;
            if r.data.is_empty() {
                return Err(String::from(if r.end { "the server closed mid-handshake" } else { "no answer in 5 s" }));
            }
            let fed = tls.feed(&r.data);
            self.flush(id, tls)?;
            fed.map_err(why)?;
        }
        let suite = tls.suite().map_or(String::from("?"), |s| alloc::format!("{s:?}"));
        tls.write(alloc::format!("GET {path} HTTP/1.0\r\nHost: {host}\r\n\r\n").as_bytes()).map_err(why)?;
        self.flush(id, tls)?;
        let mut body = Vec::new();
        while !tls.peer_closed() {
            let r = self.call::<_, nt::Received>(nt::TCP_RECV, &recv, 7)?;
            if r.data.is_empty() {
                if r.end {
                    break; // the TCP connection ended without close_notify
                }
                return Err(alloc::format!("nothing more in 5 s after {} bytes", body.len()));
            }
            tls.feed(&r.data).map_err(why)?;
            self.flush(id, tls)?;
            body.extend(tls.take_data());
        }
        tls.close();
        let _ = self.flush(id, tls);
        let first = body.split(|&b| b == b'\r' || b == b'\n').next().unwrap_or(&[]);
        Ok(alloc::vec![
            alloc::format!("tls {suite} {host}"),
            String::from_utf8_lossy(first).into_owned(),
            alloc::format!("got {} bytes", body.len()),
        ])
    }
}
