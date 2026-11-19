// SPDX-License-Identifier: Apache-2.0
//! The test door's sessions (TASK-100, e2e images only): until `accounts`
//! logs people in (TASK-101), `vfs session <user>` makes `<user>`'s sealed
//! home if it isn't there (its key derived from the name: a test key, never
//! a real account's), unlocks it, and makes `<user>` the session's account;
//! `vfs session` ends the session.

use crate::services::Conductor;
use alloc::string::String;
use alloc::vec::Vec;
use fstd::ipc::{CallError, Client};
use fstd::time::{Duration, Instant};
use fwire::proto::ffs as f;
use fwire::proto::vfs as v;

#[derive(Default)]
pub struct VfsDoor {
    ffs: Option<Client>,
    vfs: Option<Client>,
}

fn deadline() -> u64 {
    Instant::after(Duration::from_secs(10))
}

/// The test key of `user`'s home.
fn test_key(user: &str) -> [u8; 32] {
    fcrypto::sha2::sha256(alloc::format!("freally e2e test home: {user}").as_bytes())
}

impl VfsDoor {
    pub fn run(&mut self, c: &Conductor, arg: &str) -> Vec<String> {
        for (name, slot) in [("ffs", &mut self.ffs), ("vfs", &mut self.vfs)] {
            if slot.is_none() {
                *slot = c.open_connection(name, "conductor").map(Client::new);
            }
        }
        let (Some(ffs), Some(vfs)) = (self.ffs.as_mut(), self.vfs.as_mut()) else {
            return alloc::vec![String::from("error: ffs or vfs isn't up")];
        };
        let said = |r: Result<(), String>| {
            alloc::vec![r.map_or_else(|e| alloc::format!("error: {e}"), |_| String::from("ok"))]
        };
        match arg.split_whitespace().collect::<Vec<_>>().as_slice() {
            ["session"] => said(
                vfs.call::<_, ()>(v::SET_SESSION, &v::Session { user: String::new(), sv: 0 }, Vec::new(), deadline())
                    .map(|_| ())
                    .map_err(|e| alloc::format!("{e:?}")),
            ),
            ["session", user] => said((|| {
                let key = test_key(user);
                let name = alloc::format!("home:{user}");
                let sv =
                    match ffs.call::<_, f::Subvol>(f::FIND, &f::Named { name: name.clone() }, Vec::new(), deadline()) {
                        Ok((s, _)) => {
                            ffs.call::<_, ()>(f::UNLOCK, &f::Unlock { sv: s.sv, key }, Vec::new(), deadline())
                                .map_err(|e| alloc::format!("unlock: {e:?}"))?;
                            s.sv
                        }
                        Err(CallError::Remote(code, _)) if code == fcore_abi::FErr::NotFound as i32 => {
                            let home = f::Home { name, owner: [0; 16], key };
                            ffs.call::<_, f::Subvol>(f::CREATE_HOME, &home, Vec::new(), deadline())
                                .map_err(|e| alloc::format!("create: {e:?}"))?
                                .0
                                .sv
                        }
                        Err(e) => return Err(alloc::format!("find: {e:?}")),
                    };
                vfs.call::<_, ()>(v::SET_SESSION, &v::Session { user: String::from(*user), sv }, Vec::new(), deadline())
                    .map(|_| ())
                    .map_err(|e| alloc::format!("{e:?}"))
            })()),
            _ => alloc::vec![String::from("error: vfs session [user]")],
        }
    }
}
