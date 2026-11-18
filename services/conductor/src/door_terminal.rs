// SPDX-License-Identifier: Apache-2.0
//! The test door's terminal (e2e flavour only): conductor as the terminal's
//! test client (TASK-060).
//!
//! - `terminal open`: starts it (it's started on demand); `ok`, or `up` if
//!   it already runs. The door can't wait for it here (conductor serves its
//!   registration in this same loop): ask `screen` until it answers.
//! - `terminal feed <hex>`: bytes straight to its screen (a recorded sequence);
//! - `terminal type <hex>`: bytes as if typed, to the shell;
//! - `terminal screen` → `row <n> <text>` each (blank rows left out),
//!   `cursor <x> <y>`, `scrollback <n>`, `window <id>`.

use crate::services::Conductor;
use alloc::string::String;
use alloc::vec::Vec;
use fstd::ipc::Client;
use fstd::time::{Duration, Instant};
use fwire::proto::terminal as term;

#[derive(Default)]
pub struct TermDoor {
    client: Option<Client>,
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

impl TermDoor {
    pub fn run(&mut self, c: &mut Conductor, arg: &str) -> Vec<String> {
        let (op, rest) = arg.split_once(' ').unwrap_or((arg, ""));
        if op == "open" {
            return alloc::vec![String::from(if c.start_on_demand("terminal") { "ok" } else { "up" })];
        }
        if self.client.is_none() {
            self.client = c.open_connection("terminal", "conductor").map(Client::new);
        }
        let Some(cl) = self.client.as_mut() else {
            return alloc::vec![String::from("error: the terminal isn't up")];
        };
        let deadline = Instant::after(Duration::from_secs(2));
        let r = match op {
            "feed" | "type" => match unhex(rest) {
                Some(data) => {
                    let ordinal = if op == "feed" { term::FEED } else { term::TYPE };
                    cl.call::<_, ()>(ordinal, &term::Bytes { data }, Vec::new(), deadline)
                        .map(|_| alloc::vec![String::from("ok")])
                        .map_err(|e| alloc::format!("{e:?}"))
                }
                None => Err(String::from("terminal feed|type <hex>")),
            },
            "screen" => cl
                .call::<_, term::Screen>(term::SCREEN, &(), Vec::new(), deadline)
                .map(|(s, _)| {
                    let mut out: Vec<String> = s
                        .rows
                        .iter()
                        .enumerate()
                        .filter(|(_, r)| !r.is_empty())
                        .map(|(i, r)| alloc::format!("row {i} {r}"))
                        .collect();
                    out.push(alloc::format!("cursor {} {}", s.cursor.0, s.cursor.1));
                    out.push(alloc::format!("scrollback {}", s.scrollback));
                    out.push(alloc::format!("window {}", s.window));
                    out
                })
                .map_err(|e| alloc::format!("{e:?}")),
            _ => Err(String::from("terminal open|feed|type|screen")),
        };
        if r.is_err() {
            // The terminal may have gone (closed, restarted): connect afresh next time.
            self.client = None;
        }
        r.unwrap_or_else(|e| alloc::vec![alloc::format!("error: {e}")])
    }
}
