// SPDX-License-Identifier: Apache-2.0
//! The test door's TLS (e2e flavour only, TASK-180): fhttps over netstack's
//! TCP, as a client of the net protocol (the shell's `fetch` uses the same).
//! - `tls get <ip> <port> <host> <root DER in hex> <path>` connects, checks the
//!   server's chain for `<host>` against that root alone, sends fhttp's GET,
//!   reads the response with fhttp's reader: `tls <suite> <host>`, then
//!   `http <status> <reason>`, then `body <n> bytes`.
//!
//! Any failure: `error: …` (`certificate Name`, `alert …`, netstack's reason).

use crate::services::Conductor;
use alloc::string::String;
use alloc::vec::Vec;
use fstd::ipc::Client;
use fstd::time::{Duration, Instant};

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

    fn get(&mut self, to: [u8; 4], port: u16, host: &str, root: &[u8], path: &str) -> Result<Vec<String>, String> {
        let net = self.client.as_mut().ok_or("netstack isn't up")?;
        // The door's host as given (a wrong name is a test of its own).
        let url = fhttps::Url { host: String::from(host), port, path: String::from(path) };
        let (suite, r) = fhttps::get_at(net, to, &url, &[root], 1 << 20, Instant::after(Duration::from_secs(60)))?;
        Ok(alloc::vec![
            alloc::format!("tls {suite} {host}"),
            alloc::format!("http {} {}", r.status, r.reason),
            alloc::format!("body {} bytes", r.body.len()),
        ])
    }
}
