// SPDX-License-Identifier: Apache-2.0
//! The test door's accounts (TASK-101, e2e images only), until the welcome
//! and login screens call them (TASK-102): `accounts list`, `accounts create
//! <user> <password> [<owner's password>]`, `accounts login <user>
//! <password>`, `accounts logout`, `accounts passwd <old> <new>`, `accounts
//! who`. Each answers `ok` (or its lines), or `error: <code> <words>`.

use crate::services::Conductor;
use alloc::string::String;
use alloc::vec::Vec;
use fstd::ipc::{CallError, Client};
use fstd::time::{Duration, Instant};
use fwire::proto::accounts as a;

#[derive(Default)]
pub struct AccountsDoor {
    accounts: Option<Client>,
}

fn said(e: CallError) -> String {
    match e {
        CallError::Remote(code, text) => alloc::format!("error: {code} {text}"),
        e => alloc::format!("error: {e:?}"),
    }
}

impl AccountsDoor {
    pub fn run(&mut self, c: &Conductor, arg: &str) -> Vec<String> {
        if self.accounts.is_none() {
            self.accounts = c.open_connection("accounts", "conductor").map(Client::new);
        }
        let Some(acc) = self.accounts.as_mut() else {
            return alloc::vec![String::from("error: accounts isn't up")];
        };
        // Argon2id at 64 MiB takes a while in a VM.
        let deadline = Instant::after(Duration::from_secs(30));
        let s = String::from;
        let done =
            |r: Result<((), Vec<fstd::handle::Handle>), CallError>| alloc::vec![r.map_or_else(said, |_| s("ok"))];
        match arg.split_whitespace().collect::<Vec<_>>().as_slice() {
            ["list"] => match acc.call::<_, a::Users>(a::LIST, &(), Vec::new(), deadline) {
                Ok((u, _)) if u.users.is_empty() => alloc::vec![s("(none)")],
                Ok((u, _)) => u
                    .users
                    .iter()
                    .map(|u| alloc::format!("{} {}", u.username, if u.owner { "owner" } else { "standard" }))
                    .collect(),
                Err(e) => alloc::vec![said(e)],
            },
            ["create", user, password, rest @ ..] if rest.len() <= 1 => {
                let req = a::Create {
                    username: s(user),
                    display_name: s(user),
                    password: s(password),
                    owner_password: rest.first().map(|p| s(p)).unwrap_or_default(),
                };
                done(acc.call(a::CREATE, &req, Vec::new(), deadline))
            }
            ["login", user, password] => {
                done(acc.call(a::LOGIN, &a::Login { username: s(user), password: s(password) }, Vec::new(), deadline))
            }
            ["logout"] => done(acc.call(a::LOGOUT, &(), Vec::new(), deadline)),
            ["passwd", old, new] => done(acc.call(
                a::CHANGE_PASSWORD,
                &a::ChangePassword { old: s(old), new: s(new) },
                Vec::new(),
                deadline,
            )),
            ["who"] => match acc.call::<_, a::Who>(a::SESSION, &(), Vec::new(), deadline) {
                Ok((w, _)) if w.user.username.is_empty() => alloc::vec![s("(no one)")],
                Ok((w, _)) => alloc::vec![w.user.username],
                Err(e) => alloc::vec![said(e)],
            },
            _ => alloc::vec![s(
                "error: accounts list|create <user> <password> [<owner's password>]|login <user> <password>|logout|passwd <old> <new>|who"
            )],
        }
    }
}
