// SPDX-License-Identifier: Apache-2.0
//! accounts, the service (TASK-101, PRD § 10): the records under
//! `/system/accounts` (through vfs, which lets accounts alone write there),
//! account creation (the home made sealed by ffs), login (the password's
//! KEK unwraps the master key, ffs unlocks the home with it, vfs makes the
//! account the session's), logout (ffs locks the home and forgets the key,
//! the session ends, the key here is zeroed), password changes (the key
//! re-wrapped; the files untouched), and the throttle.
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::{FErr, signal, topic};
use fstd::ipc::{Channel, Client};
use fstd::time::{Duration, Instant};
use fwire::proto::accounts as a;
use fwire::proto::conductor as c;
use fwire::proto::ffs as f;
use fwire::proto::vfs as v;
use svc_accounts::{AccountRecord, Refused, Role, Throttle, file_name, kek, unwrap, wipe, wrap};

fstd::program!(run);

const DIR: &str = "/system/accounts";

type Reply = Result<Vec<u8>, (FErr, String)>;

/// The logged-in account and the master key held for it.
struct Session {
    username: String,
    home: u64,
    key: [u8; 32],
}

struct Accounts {
    ffs: Client,
    vfs: Client,
    records: Vec<AccountRecord>,
    session: Option<Session>,
    throttle: Throttle,
    /// No try is heard before this (monotonic ns).
    next_try: u64,
}

fn refused(r: Refused) -> (FErr, String) {
    match r {
        Refused::WrongPassword => (FErr::AccessDenied, String::from("That password didn't match.")),
        Refused::ShortPassword => (FErr::InvalidArgs, String::from("A password needs at least 8 characters.")),
        Refused::BadUsername => (
            FErr::InvalidArgs,
            String::from("A username is 2 to 32 lowercase letters, digits, - or _, starting with a letter."),
        ),
        Refused::BadDisplayName => (FErr::InvalidArgs, String::from("A display name is at most 64 characters.")),
        Refused::Damaged => (FErr::Integrity, String::from("An account record is damaged.")),
        Refused::Kdf => (FErr::NoMemory, String::from("There isn't enough memory to check the password.")),
    }
}

fn random<const N: usize>() -> Result<[u8; N], (FErr, String)> {
    let mut b = [0u8; N];
    frandom::fill(&mut b).map_err(|_| (FErr::Internal, String::from("no randomness (RDRAND/RDSEED refused)")))?;
    Ok(b)
}

/// A new account's master key: random. In e2e images it's derived from the
/// username instead, so `@accounts.key_gone` knows what to look for in the
/// machine's RAM (an e2e image's accounts are never anyone's real ones).
fn master_key(username: &str) -> Result<[u8; 32], (FErr, String)> {
    #[cfg(feature = "flavour-e2e")]
    return Ok(fcrypto::sha2::sha256(alloc::format!("freally e2e master key: {username}").as_bytes()));
    #[cfg(not(feature = "flavour-e2e"))]
    {
        let _ = username;
        random::<32>()
    }
}

/// PRD § 10: Argon2id at 64 MiB; 32 MiB on a machine under 2 GiB of RAM.
fn kdf() -> fcrypto::argon2::Params {
    let mut b = [0u8; 16];
    let total = fstd::sys::system_info(topic::MEMORY, &mut b)
        .map(|_| u64::from_le_bytes(b[8..].try_into().unwrap_or_default()));
    if total.unwrap_or(0) < 2 << 30 { svc_accounts::SMALL_KDF } else { svc_accounts::DEFAULT_KDF }
}

impl Accounts {
    fn call<Req: fwire::Wire, Rep: fwire::Wire>(
        c: &mut Client,
        ordinal: u32,
        req: &Req,
    ) -> Result<Rep, (FErr, String)> {
        c.call::<_, Rep>(ordinal, req, Vec::new(), Instant::after(Duration::from_secs(15))).map(|(r, _)| r).map_err(
            |e| match e {
                fstd::ipc::CallError::Remote(code, text) => (FErr::from_code(code as i64), text),
                other => (FErr::Internal, alloc::format!("{other:?}")),
            },
        )
    }

    /// The records under `/system/accounts`, each checked.
    fn load(&mut self) {
        self.records.clear();
        let list = match fstd::fs::read_dir(DIR) {
            Ok(list) => list,
            // A new disk: the folder the records go in.
            Err(e) if e.code == FErr::NotFound => {
                if let Err(e) = fstd::fs::create_dir(DIR) {
                    fstd::error!("accounts: making {DIR}: {}", e.describe());
                }
                return;
            }
            Err(e) => {
                fstd::error!("accounts: reading {DIR}: {}", e.describe());
                return;
            }
        };
        for (name, dir) in list {
            if dir || !name.ends_with(".rec") {
                continue;
            }
            match fstd::fs::read(&alloc::format!("{DIR}/{name}")).map(|b| AccountRecord::decode(&b)) {
                Ok(Ok(r)) => self.records.push(r),
                _ => fstd::error!("accounts: {DIR}/{name} is damaged: left out"),
            }
        }
    }

    fn save(&self, r: &AccountRecord) -> Result<(), (FErr, String)> {
        fstd::fs::write(&alloc::format!("{DIR}/{}", file_name(&r.id)), &r.encode()).map_err(|e| (e.code, e.text))
    }

    fn create(&mut self, cr: &a::Create) -> Reply {
        if !svc_accounts::valid_username(&cr.username) {
            return Err(refused(Refused::BadUsername));
        }
        if cr.display_name.chars().count() > 64 {
            return Err(refused(Refused::BadDisplayName));
        }
        svc_accounts::check_password(&cr.password).map_err(refused)?;
        if self.records.iter().any(|r| r.username == cr.username) {
            return Err((FErr::AlreadyExists, alloc::format!("There's already an account called {}.", cr.username)));
        }
        // The first account is the Owner; any other needs the Owner, logged in, to say so.
        let role = if self.records.is_empty() {
            Role::Owner
        } else {
            let owner = self.session.as_ref().and_then(|s| self.records.iter().find(|r| r.username == s.username));
            let Some(owner) = owner.filter(|r| r.role == Role::Owner) else {
                return Err((FErr::AccessDenied, String::from("Only the Owner adds accounts.")));
            };
            let k = kek(&cr.owner_password, &owner.salt, owner.kdf).map_err(refused)?;
            let mut check = unwrap(&k, &owner.wrap_nonce, &owner.id, &owner.wrapped_master_key).map_err(refused)?;
            wipe(&mut check);
            Role::Standard
        };
        let (id, salt, nonce) = (random::<16>()?, random::<16>()?, random::<24>()?);
        let mut master = master_key(&cr.username)?;
        let params = kdf();
        let mut k = kek(&cr.password, &salt, params).map_err(refused)?;
        let wrapped = wrap(&k, &nonce, &id, &master);
        wipe(&mut k);
        let mut home = f::Home { name: alloc::format!("home:{}", cr.username), owner: id, key: master };
        let made: Result<f::Subvol, _> = Self::call(&mut self.ffs, f::CREATE_HOME, &home);
        wipe(&mut home.key);
        wipe(&mut master);
        let sv = made?.sv;
        // Made unlocked: locked until its owner logs in.
        Self::call::<_, ()>(&mut self.ffs, f::LOCK, &f::Subvol { sv })?;
        let r = AccountRecord {
            id,
            username: cr.username.clone(),
            display_name: cr.display_name.clone(),
            role,
            created_unix: fstd::time::utc_ns() / 1_000_000_000,
            kdf: params,
            salt,
            wrapped_master_key: wrapped,
            wrap_nonce: nonce,
            home_subvolume: sv,
            dark: true,
            ui_scale_pct: 100,
            locale: *b"en-US\0\0\0",
        };
        self.save(&r)?;
        fstd::info!("accounts: made {} ({:?}), home subvolume {sv}", cr.username, role);
        self.records.push(r);
        Ok(fwire::to_payload(&()))
    }

    fn login(&mut self, l: &a::Login) -> Reply {
        let now = Instant::now().as_ns();
        if now < self.next_try {
            return Err((FErr::Busy, String::from("Wait a moment before trying again.")));
        }
        if self.session.is_some() {
            return Err((FErr::BadState, String::from("Someone is logged in: log out first.")));
        }
        let Some(r) = self.records.iter().find(|r| r.username == l.username).cloned() else {
            // The same words, the same wait, as a wrong password.
            self.next_try = now + self.throttle.failed();
            return Err(refused(Refused::WrongPassword));
        };
        let mut k = kek(&l.password, &r.salt, r.kdf).map_err(refused)?;
        let opened = unwrap(&k, &r.wrap_nonce, &r.id, &r.wrapped_master_key);
        wipe(&mut k);
        let Ok(key) = opened else {
            self.next_try = Instant::now().as_ns() + self.throttle.failed();
            fstd::warn!("accounts: a wrong password for {} ({} in a row)", r.username, self.throttle.failures);
            return Err(refused(Refused::WrongPassword));
        };
        self.throttle.succeeded();
        let mut unlock = f::Unlock { sv: r.home_subvolume, key };
        let unlocked = Self::call::<_, ()>(&mut self.ffs, f::UNLOCK, &unlock);
        wipe(&mut unlock.key);
        if let Err(e) = unlocked {
            let mut key = key;
            wipe(&mut key);
            return Err(e);
        }
        Self::call::<_, ()>(
            &mut self.vfs,
            v::SET_SESSION,
            &v::Session { user: r.username.clone(), sv: r.home_subvolume },
        )?;
        fstd::info!("accounts: {} logged in", r.username);
        self.session = Some(Session { username: r.username, home: r.home_subvolume, key });
        Ok(fwire::to_payload(&()))
    }

    fn logout(&mut self) -> Reply {
        // Wiped where it lies: taking it out first would leave the bytes behind.
        let Some(s) = self.session.as_mut() else { return Ok(fwire::to_payload(&())) };
        wipe(&mut s.key);
        let Some(s) = self.session.take() else { return Ok(fwire::to_payload(&())) };
        let locked = Self::call::<_, ()>(&mut self.ffs, f::LOCK, &f::Subvol { sv: s.home });
        let ended = Self::call::<_, ()>(&mut self.vfs, v::SET_SESSION, &v::Session { user: String::new(), sv: 0 });
        fstd::info!("accounts: {} logged out", s.username);
        locked.and(ended).map(|_| fwire::to_payload(&()))
    }

    fn change_password(&mut self, cp: &a::ChangePassword) -> Reply {
        let user = self
            .session
            .as_ref()
            .map(|s| s.username.clone())
            .ok_or((FErr::AccessDenied, String::from("Log in first.")))?;
        svc_accounts::check_password(&cp.new).map_err(refused)?;
        let i =
            self.records.iter().position(|r| r.username == user).ok_or((FErr::NotFound, String::from("no record")))?;
        let r = self.records[i].clone();
        let mut old = kek(&cp.old, &r.salt, r.kdf).map_err(refused)?;
        let opened = unwrap(&old, &r.wrap_nonce, &r.id, &r.wrapped_master_key);
        wipe(&mut old);
        let mut master = opened.map_err(refused)?;
        let (salt, nonce) = (random::<16>()?, random::<24>()?);
        let params = kdf();
        let mut new = kek(&cp.new, &salt, params).map_err(refused)?;
        let wrapped = wrap(&new, &nonce, &r.id, &master);
        wipe(&mut new);
        wipe(&mut master);
        let updated = AccountRecord { salt, wrap_nonce: nonce, wrapped_master_key: wrapped, kdf: params, ..r };
        self.save(&updated)?;
        self.records[i] = updated;
        fstd::info!("accounts: {user}'s password changed");
        Ok(fwire::to_payload(&()))
    }

    fn handle(&mut self, who: &str, ordinal: u32, p: &[u8]) -> Reply {
        fn arg<T: fwire::Wire>(p: &[u8]) -> Result<T, (FErr, String)> {
            fwire::from_payload(p).map_err(|e| (FErr::InvalidArgs, String::from(e.describe())))
        }
        let _ = who;
        match ordinal {
            a::LIST => Ok(fwire::to_payload(&a::Users {
                users: self
                    .records
                    .iter()
                    .map(|r| a::User {
                        username: r.username.clone(),
                        display_name: r.display_name.clone(),
                        owner: r.role == Role::Owner,
                    })
                    .collect(),
            })),
            a::CREATE => {
                let mut cr: a::Create = arg(p)?;
                let r = self.create(&cr);
                fstd::secret::wipe_str(&mut cr.password);
                fstd::secret::wipe_str(&mut cr.owner_password);
                r
            }
            a::LOGIN => {
                let mut l: a::Login = arg(p)?;
                let r = self.login(&l);
                fstd::secret::wipe_str(&mut l.password);
                r
            }
            a::LOGOUT => self.logout(),
            a::CHANGE_PASSWORD => {
                let mut cp: a::ChangePassword = arg(p)?;
                let r = self.change_password(&cp);
                fstd::secret::wipe_str(&mut cp.old);
                fstd::secret::wipe_str(&mut cp.new);
                r
            }
            a::SESSION => {
                let user = match &self.session {
                    Some(s) => {
                        let r = self.records.iter().find(|r| r.username == s.username);
                        a::User {
                            username: s.username.clone(),
                            display_name: r.map(|r| r.display_name.clone()).unwrap_or_default(),
                            owner: r.is_some_and(|r| r.role == Role::Owner),
                        }
                    }
                    None => a::User { username: String::new(), display_name: String::new(), owner: false },
                };
                Ok(fwire::to_payload(&a::Who { user }))
            }
            _ => Err((FErr::NotSupported, String::from("accounts doesn't serve that"))),
        }
    }
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    let connect = |name: &str| fstd::service::connect_within(name, Duration::from_secs(30)).ok().map(Client::new);
    let (Some(ffs), Some(vfs)) = (connect("ffs"), connect("vfs")) else {
        fstd::info!("accounts: no storage: no accounts (a guest session only)");
        return 0;
    };
    let mut acc = Accounts { ffs, vfs, records: Vec::new(), session: None, throttle: Throttle::default(), next_try: 0 };
    acc.load();
    fstd::info!(
        "accounts: {} account(s){}",
        acc.records.len(),
        if acc.records.is_empty() { ": the welcome screen makes the first" } else { "" }
    );
    let Ok((listen, give)) = Channel::pair() else { return 1 };
    if fstd::service::register(give).is_err() {
        return 1;
    }
    let mut clients: Vec<(String, Channel)> = Vec::new();
    loop {
        let mut items = alloc::vec![[listen.0.raw(), signal::READABLE]];
        items.extend(clients.iter().map(|(_, ch)| [ch.0.raw(), signal::READABLE | signal::PEER_CLOSED]));
        match fstd::sys::wait_many(&items, u64::MAX) {
            Ok(0) => {
                if let Ok(m) = listen.recv(0)
                    && let Ok((h, p)) = fwire::parse(&m.bytes)
                    && h.ordinal == c::INCOMING
                    && let Ok(inc) = fwire::from_payload::<c::Incoming>(p)
                    && let Some(ch) = m.handles.into_iter().next()
                {
                    clients.push((inc.client, Channel(ch)));
                }
            }
            Ok(k) => {
                let (who, ch) = &clients[k - 1];
                let who = who.clone();
                if !serve(&mut acc, &who, ch) {
                    clients.remove(k - 1);
                }
            }
            Err(_) => fstd::time::sleep(Duration::from_millis(10)),
        }
    }
}

fn serve(acc: &mut Accounts, who: &str, ch: &Channel) -> bool {
    loop {
        let mut m = match ch.try_recv() {
            Ok(m) => m,
            Err(FErr::ShouldWait) => return true,
            Err(_) => return false,
        };
        if fstd::inventory::seen(ch, &m.bytes) {
            continue;
        }
        let Ok((h, p)) = fwire::parse(&m.bytes) else { continue };
        let (txid, ordinal) = (h.txid, h.ordinal);
        let msg = match acc.handle(who, ordinal, p) {
            Ok(body) => fwire::message_bytes(txid, ordinal, fwire::flags::REPLY, body),
            Err((e, text)) => fwire::error_reply(txid, ordinal, e as i32, &text),
        };
        if let Ok(msg) = msg {
            let _ = ch.send(&msg, Vec::new());
        }
        // A password came in, or a key was used: the request is wiped, and
        // so is the stack the work used (FR-046).
        if ordinal != a::LIST && ordinal != a::SESSION {
            fstd::secret::wipe(&mut m.bytes);
            fstd::secret::scrub_stack();
        }
    }
}
