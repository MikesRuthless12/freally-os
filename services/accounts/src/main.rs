// SPDX-License-Identifier: Apache-2.0
//! accounts, the service (TASK-101, PRD § 10): the records under
//! `/system/accounts` (through vfs, which lets accounts alone write there),
//! account creation (the home made sealed by ffs), login (the password's
//! KEK unwraps the master key, ffs unlocks the home with it, vfs makes the
//! account the session's), logout (ffs locks the home and forgets the key,
//! the session ends, the key here is zeroed), password changes (the key
//! re-wrapped; the files untouched), and the throttle. From TASK-104, each
//! account's look (put on the screen at login and when Settings saves it)
//! and the Owner removing an account.
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
use fwire::proto::stage as st;
use fwire::proto::vfs as v;
use svc_accounts::{AccountRecord, Refused, Role, Throttle, file_name, kek, unwrap, wipe, wrap};

mod bundle;

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
    /// stage, once a look is first put on the screen.
    stage: Option<Client>,
    /// A look to put on the screen once the request that brought it is
    /// answered: reaching stage means asking conductor, which may be the
    /// one waiting on that request (its test door).
    to_show: Option<AccountRecord>,
    /// The clipboard to empty once the login or logout is answered (as
    /// `to_show`: never while conductor may be waiting on the reply).
    to_clear: bool,
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

    /// A new account's name and password, before anything else is asked.
    fn check_new(&self, username: &str, display_name: &str, password: &str) -> Result<(), (FErr, String)> {
        if !svc_accounts::valid_username(username) {
            return Err(refused(Refused::BadUsername));
        }
        svc_accounts::check_display_name(display_name).map_err(refused)?;
        svc_accounts::check_password(password).map_err(refused)?;
        if self.records.iter().any(|r| r.username == username) {
            return Err((FErr::AlreadyExists, alloc::format!("There's already an account called {username}.")));
        }
        Ok(())
    }

    /// The role of a new account: the first is the Owner; any other needs
    /// the Owner, logged in, to give its password.
    fn role_for_new(&mut self, owner_password: &str) -> Result<Role, (FErr, String)> {
        if self.records.is_empty() {
            return Ok(Role::Owner);
        }
        self.owner_check(owner_password, "Only the Owner adds accounts.")?;
        Ok(Role::Standard)
    }

    /// The Owner, logged in, gives its password (throttled as logins are).
    fn owner_check(&mut self, owner_password: &str, only: &str) -> Result<(), (FErr, String)> {
        let now = Instant::now().as_ns();
        if now < self.next_try {
            return Err((FErr::Busy, String::from("Wait a moment before trying again.")));
        }
        let owner = self.session.as_ref().and_then(|s| self.records.iter().find(|r| r.username == s.username));
        let Some(owner) = owner.filter(|r| r.role == Role::Owner) else {
            return Err((FErr::AccessDenied, String::from(only)));
        };
        let mut k = kek(owner_password, &owner.salt, owner.kdf).map_err(refused)?;
        let opened = unwrap(&k, &owner.wrap_nonce, &owner.id, &owner.wrapped_master_key);
        wipe(&mut k);
        let Ok(mut check) = opened else {
            self.next_try = now + self.throttle.failed();
            return Err(refused(Refused::WrongPassword));
        };
        wipe(&mut check);
        self.throttle.succeeded();
        Ok(())
    }

    /// TASK-104: the Owner removes another account. Its record goes, and its
    /// home is retired: still sealed, and its key gone with the record.
    fn remove(&mut self, rm: &a::Remove) -> Reply {
        self.owner_check(&rm.owner_password, "Only the Owner removes accounts.")?;
        if rm.confirm != rm.username {
            return Err((FErr::InvalidArgs, String::from("Type the username again to remove the account.")));
        }
        let i = self
            .records
            .iter()
            .position(|r| r.username == rm.username)
            .ok_or((FErr::NotFound, alloc::format!("There's no account called {}.", rm.username)))?;
        let r = self.records[i].clone();
        if r.role == Role::Owner {
            return Err((FErr::AccessDenied, String::from("The Owner's account can't be removed.")));
        }
        Self::call::<_, ()>(&mut self.ffs, f::RETIRE, &f::Subvol { sv: r.home_subvolume })?;
        fstd::fs::remove(&alloc::format!("{DIR}/{}", file_name(&r.id))).map_err(|e| (e.code, e.text))?;
        self.records.remove(i);
        fstd::info!("accounts: {} removed (home subvolume {} retired)", r.username, r.home_subvolume);
        Ok(fwire::to_payload(&()))
    }

    /// TASK-104: Settings saves the session account's look; it's put on the
    /// screen at once.
    fn set_look(&mut self, l: &st::Look) -> Reply {
        let user = self
            .session
            .as_ref()
            .map(|s| s.username.clone())
            .ok_or((FErr::AccessDenied, String::from("Log in first.")))?;
        let saver_ok = l.saver < 4 || l.saver == st::NO_SAVER;
        let scale_ok = l.scale_pct == 0 || ((100..=300).contains(&l.scale_pct) && l.scale_pct % 25 == 0);
        if (l.wallpaper as usize) >= st::WALLPAPERS.len()
            || !saver_ok
            || !scale_ok
            || !(1..=120).contains(&l.saver_idle_min)
        {
            return Err((FErr::InvalidArgs, String::from("That look isn't one Settings offers.")));
        }
        let i =
            self.records.iter().position(|r| r.username == user).ok_or((FErr::NotFound, String::from("no record")))?;
        let mut r = self.records[i].clone();
        r.dark = l.dark;
        r.ui_scale_pct = l.scale_pct;
        r.look = svc_accounts::Look {
            wallpaper: l.wallpaper,
            reduce_motion: l.reduce_motion,
            clock_24h: l.clock_24h,
            saver: l.saver,
            saver_idle_min: l.saver_idle_min,
        };
        self.save(&r)?;
        self.to_show = Some(r.clone());
        self.records[i] = r;
        fstd::info!("accounts: {user}'s look saved");
        Ok(fwire::to_payload(&()))
    }

    /// stage, connected the first time it's needed.
    fn stage(&mut self) -> Option<&mut Client> {
        if self.stage.is_none() {
            match fstd::service::connect("stage") {
                Ok(ch) => self.stage = Some(Client::new(ch)),
                Err(e) => fstd::warn!("accounts: stage: {e:?}"),
            }
        }
        self.stage.as_mut()
    }

    /// The clipboard emptied as a session starts or ends: nothing one
    /// person copied reaches the next.
    fn clear_clipboard(&mut self) {
        let Some(stage) = self.stage() else { return };
        if let Err(e) = Self::call::<_, ()>(stage, st::CLEAR_CLIPBOARD, &()) {
            fstd::warn!("accounts: the clipboard wasn't emptied: {}", e.1);
        }
    }

    /// An account's look put on the screen (after a login, and when it's
    /// saved), between requests.
    fn show(&mut self, r: &AccountRecord) {
        let Some(stage) = self.stage() else { return };
        let l = r.look;
        let theme = if r.dark { st::Theme::Dark } else { st::Theme::Light };
        let wall = st::WALLPAPERS.get(l.wallpaper as usize).copied().unwrap_or("studio");
        let idle = if l.saver_idle_min == 0 { svc_accounts::SAVER_IDLE_DEFAULT_MIN } else { l.saver_idle_min };
        let saver = if l.saver == st::NO_SAVER {
            st::SetSaver { kind: 0, idle_ms: 0 }
        } else {
            st::SetSaver { kind: l.saver.min(3), idle_ms: idle as u32 * 60_000 }
        };
        let told = [
            Self::call::<_, ()>(stage, st::SET_THEME, &st::SetTheme { theme }),
            Self::call::<_, ()>(stage, st::SET_SCALE, &st::SetScale { pct: r.ui_scale_pct }),
            Self::call::<_, ()>(stage, st::SET_WALLPAPER, &st::SetWallpaper { name: String::from(wall) }),
            Self::call::<_, ()>(stage, st::SET_MOTION, &st::SetMotion { reduce: l.reduce_motion }),
            Self::call::<_, ()>(stage, st::SET_CLOCK, &st::SetClock { h24: l.clock_24h }),
            Self::call::<_, ()>(stage, st::SET_SAVER, &saver),
        ];
        for e in told.into_iter().filter_map(Result::err) {
            fstd::warn!("accounts: {}'s look: {}", r.username, e.1);
        }
    }

    /// A new account with its sealed home, which is left unlocked for the
    /// caller to fill, lock and save; `like` gives an imported account its
    /// settings and when it was made.
    fn make(
        &mut self,
        username: &str,
        display_name: &str,
        password: &str,
        role: Role,
        like: Option<&AccountRecord>,
    ) -> Result<AccountRecord, (FErr, String)> {
        self.check_new(username, display_name, password)?;
        let (id, salt, nonce) = (random::<16>()?, random::<16>()?, random::<24>()?);
        let mut master = master_key(username)?;
        let params = kdf();
        let mut k = kek(password, &salt, params).map_err(refused)?;
        let wrapped = wrap(&k, &nonce, &id, &master);
        wipe(&mut k);
        let mut home = f::Home { name: alloc::format!("home:{username}"), owner: id, key: master };
        let made: Result<f::Subvol, _> = Self::call(&mut self.ffs, f::CREATE_HOME, &home);
        wipe(&mut home.key);
        wipe(&mut master);
        let sv = made?.sv;
        Ok(AccountRecord {
            id,
            username: String::from(username),
            display_name: String::from(display_name),
            role,
            created_unix: like.map_or(fstd::time::utc_ns() / 1_000_000_000, |l| l.created_unix),
            kdf: params,
            salt,
            wrapped_master_key: wrapped,
            wrap_nonce: nonce,
            home_subvolume: sv,
            dark: like.is_none_or(|l| l.dark),
            ui_scale_pct: like.map_or(0, |l| l.ui_scale_pct),
            locale: like.map_or(*b"en-US\0\0\0", |l| l.locale),
            look: like.map(|l| l.look).unwrap_or_default(),
        })
    }

    /// The home locked (until its owner logs in) and the record kept.
    fn keep(&mut self, r: AccountRecord) -> Result<(), (FErr, String)> {
        Self::call::<_, ()>(&mut self.ffs, f::LOCK, &f::Subvol { sv: r.home_subvolume })?;
        self.save(&r)?;
        self.records.push(r);
        Ok(())
    }

    fn create(&mut self, cr: &a::Create) -> Reply {
        self.check_new(&cr.username, &cr.display_name, &cr.password)?;
        let role = self.role_for_new(&cr.owner_password)?;
        let r = self.make(&cr.username, &cr.display_name, &cr.password, role, None)?;
        let sv = r.home_subvolume;
        self.keep(r)?;
        fstd::info!("accounts: made {} ({:?}), home subvolume {sv}", cr.username, role);
        Ok(fwire::to_payload(&()))
    }

    /// The session's home, if someone is logged in.
    fn home(&self) -> Option<u64> {
        self.session.as_ref().map(|s| s.home)
    }

    /// TASK-103: the session's account and home, sealed with its password.
    fn export(&mut self, e: &a::Export) -> Reply {
        let s = self.session.as_ref().ok_or((FErr::AccessDenied, String::from("Log in first.")))?;
        let (user, sv) = (s.username.clone(), s.home);
        let r = self
            .records
            .iter()
            .find(|r| r.username == user)
            .cloned()
            .ok_or((FErr::NotFound, String::from("no record")))?;
        // The password typed again: the bundle opens with it.
        let mut k = kek(&e.password, &r.salt, r.kdf).map_err(refused)?;
        let opened = unwrap(&k, &r.wrap_nonce, &r.id, &r.wrapped_master_key);
        wipe(&mut k);
        let mut check = opened.map_err(refused)?;
        wipe(&mut check);
        let (salt, prefix) = (random::<16>()?, random::<16>()?);
        let now = fstd::time::utc_ns() / 1_000_000_000;
        let bytes = bundle::export(&mut self.ffs, sv, &r, &e.password, kdf(), salt, prefix, now)?;
        let path = alloc::format!("/home/{user}/{}/{user}.facct", bundle::EXPORTS);
        fstd::info!("accounts: exported {user} to {path} ({bytes} bytes)");
        Ok(fwire::to_payload(&a::Exported { path, bytes }))
    }

    fn preview(&mut self, o: &a::Open) -> Reply {
        let home = self.home();
        let found = bundle::preview(&mut self.ffs, &o.name, home, &o.password)?;
        Ok(fwire::to_payload(&a::Preview {
            username: found.record.username,
            display_name: found.record.display_name,
            files: found.files,
            bytes: found.bytes,
            exported_unix: found.exported_unix,
        }))
    }

    /// TASK-103: a bundle, checked whole first, made into an account with
    /// the bundle's password.
    fn import(&mut self, im: &a::Import) -> Reply {
        let home = self.home();
        let found = bundle::preview(&mut self.ffs, &im.name, home, &im.password)?;
        let username = if im.rename.is_empty() { found.record.username.clone() } else { im.rename.clone() };
        self.check_new(&username, &found.record.display_name, &im.password)?;
        let role = self.role_for_new(&im.owner_password)?;
        let r = self.make(&username, &found.record.display_name, &im.password, role, Some(&found.record))?;
        let sv = r.home_subvolume;
        if let Err(e) = bundle::unpack(&mut self.ffs, &im.name, home, &im.password, sv) {
            // Checked whole a moment ago: only a disk failing gets here.
            let _ = Self::call::<_, ()>(&mut self.ffs, f::LOCK, &f::Subvol { sv });
            return Err(e);
        }
        self.keep(r)?;
        fstd::info!(
            "accounts: imported {username} ({:?}) from {}: {} files, {} bytes",
            role,
            im.name,
            found.files,
            found.bytes
        );
        Ok(fwire::to_payload(&a::User {
            username,
            display_name: found.record.display_name,
            owner: role == Role::Owner,
        }))
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
        self.to_clear = true;
        fstd::info!("accounts: {} logged in", r.username);
        self.to_show = Some(r.clone());
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
        self.to_clear = true;
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
        if !svc_accounts::may(who, ordinal) {
            return Err((FErr::AccessDenied, alloc::format!("{who} may not ask accounts for that")));
        }
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
            a::EXPORT => {
                let mut e: a::Export = arg(p)?;
                let r = self.export(&e);
                fstd::secret::wipe_str(&mut e.password);
                r
            }
            a::BUNDLES => {
                let home = self.home();
                Ok(fwire::to_payload(&a::Bundles { names: bundle::bundles(&mut self.ffs, home)? }))
            }
            a::PREVIEW => {
                let mut o: a::Open = arg(p)?;
                let r = self.preview(&o);
                fstd::secret::wipe_str(&mut o.password);
                r
            }
            a::IMPORT => {
                let mut im: a::Import = arg(p)?;
                let r = self.import(&im);
                fstd::secret::wipe_str(&mut im.password);
                fstd::secret::wipe_str(&mut im.owner_password);
                r
            }
            a::SET_LOOK => self.set_look(&arg(p)?),
            a::REMOVE => {
                let mut rm: a::Remove = arg(p)?;
                let r = self.remove(&rm);
                fstd::secret::wipe_str(&mut rm.owner_password);
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
    let mut acc = Accounts {
        ffs,
        vfs,
        records: Vec::new(),
        session: None,
        throttle: Throttle::default(),
        next_try: 0,
        stage: None,
        to_show: None,
        to_clear: false,
    };
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
                if core::mem::take(&mut acc.to_clear) {
                    acc.clear_clipboard();
                }
                if let Some(r) = acc.to_show.take() {
                    acc.show(&r);
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
