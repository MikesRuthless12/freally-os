// SPDX-License-Identifier: Apache-2.0
//! The test door's Monitor (e2e flavour only): `monitor open` starts it
//! (`ok`, or `up`); `monitor state` → `tab <name>`, `sort <col> up|down`,
//! `filter <service> <level>`, `line <text>` each, `window <id>`.

use crate::services::Conductor;
use alloc::string::String;
use alloc::vec::Vec;
use fstd::ipc::Client;
use fstd::time::{Duration, Instant};
use fwire::proto::monitor as mon;

#[derive(Default)]
pub struct MonDoor {
    client: Option<Client>,
}

impl MonDoor {
    pub fn run(&mut self, c: &mut Conductor, arg: &str) -> Vec<String> {
        if arg == "open" {
            return alloc::vec![String::from(if c.start_on_demand("monitor") { "ok" } else { "up" })];
        }
        if arg != "state" {
            return alloc::vec![String::from("error: monitor open|state")];
        }
        if self.client.is_none() {
            self.client = c.open_connection("monitor", "conductor").map(Client::new);
        }
        let Some(cl) = self.client.as_mut() else {
            return alloc::vec![String::from("error: Monitor isn't up")];
        };
        match cl.call::<_, mon::State>(mon::STATE, &(), Vec::new(), Instant::after(Duration::from_secs(2))) {
            Ok((s, _)) => {
                let mut out = alloc::vec![
                    alloc::format!("tab {}", s.tab),
                    alloc::format!("sort {}", s.sort),
                    alloc::format!("filter {}", s.filter)
                ];
                out.extend(s.lines.iter().map(|l| alloc::format!("line {l}")));
                out.push(alloc::format!("window {}", s.window));
                out
            }
            Err(e) => {
                self.client = None;
                alloc::vec![alloc::format!("error: {e:?}")]
            }
        }
    }
}
