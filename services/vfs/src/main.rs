// SPDX-License-Identifier: Apache-2.0
//! vfs, the service (TASK-100, FR-043): the file protocol for every
//! process, by path, through ffs. Each request is checked against the
//! rules in the library (the caller conductor named, and the session's
//! account): `/system` read-only, each home its account's alone. Open
//! files are handles with a place, per connection.
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::{FErr, signal};
use fstd::ipc::{Channel, Client};
use fstd::time::{Duration, Instant};
use fwire::proto::conductor as c;
use fwire::proto::ffs as f;
use fwire::proto::vfs as v;
use svc_vfs::{Access, Caller, Place, check, homes_listed, parse};

fstd::program!(run);

/// The system subvolume and every subvolume's root directory.
const SYSTEM: u64 = 256;
const ROOT_INO: u64 = 256;

type Reply = Result<Vec<u8>, (FErr, String)>;

/// An open file: where it is, its place, whether it was opened to write.
struct Open {
    id: u32,
    sv: u64,
    ino: u64,
    at: u64,
    write: bool,
}

struct Conn {
    who: String,
    ch: Channel,
    files: Vec<Open>,
    next: u32,
}

struct Vfs {
    ffs: Client,
    /// The session's account and its home's subvolume.
    session: Option<(String, u64)>,
}

fn denied(e: FErr) -> (FErr, String) {
    let why = match e {
        FErr::AccessDenied => "not yours to reach",
        FErr::InvalidArgs => "not a path vfs takes (absolute, no . or ..)",
        FErr::NotFound => "no such place",
        _ => "refused",
    };
    (e, String::from(why))
}

impl Vfs {
    fn ffs<Req: fwire::Wire, Rep: fwire::Wire>(&mut self, ordinal: u32, req: &Req) -> Result<Rep, (FErr, String)> {
        self.ffs
            .call::<_, Rep>(ordinal, req, Vec::new(), Instant::after(Duration::from_secs(10)))
            .map(|(r, _)| r)
            .map_err(|e| match e {
                fstd::ipc::CallError::Remote(code, text) => (FErr::from_code(code as i64), text),
                other => (FErr::Internal, alloc::format!("ffs: {other:?}")),
            })
    }

    fn caller<'a>(&'a self, who: &'a str) -> Caller<'a> {
        Caller { service: who, session: self.session.as_ref().map(|s| s.0.as_str()) }
    }

    /// The subvolume and the directory a place's components start from.
    fn base(&self, place: &Place) -> Result<(u64, u64, Vec<String>), (FErr, String)> {
        match place {
            Place::System(parts) => Ok((SYSTEM, ROOT_INO, parts.clone())),
            Place::Home(_, parts) => {
                let sv = self.session.as_ref().map(|s| s.1).ok_or(denied(FErr::AccessDenied))?;
                Ok((sv, ROOT_INO, parts.clone()))
            }
            Place::Root | Place::Homes => Err((FErr::WrongType, String::from("a directory of mounts"))),
        }
    }

    /// The inode a path names, and its kind.
    fn resolve(&mut self, who: &str, path: &str, access: Access) -> Result<(u64, u64, u8), (FErr, String)> {
        let place = parse(path).map_err(denied)?;
        check(&self.caller(who), &place, access).map_err(denied)?;
        let (sv, mut ino, parts) = self.base(&place)?;
        let mut kind = f::DIR;
        for name in parts {
            if kind != f::DIR {
                return Err((FErr::WrongType, String::from("a file isn't a directory")));
            }
            let e: f::Entry = self.ffs(f::LOOKUP, &f::Name { sv, dir: ino, name })?;
            (ino, kind) = (e.ino, e.kind);
        }
        Ok((sv, ino, kind))
    }

    /// The directory holding a path's last component, and that name.
    fn parent(&mut self, who: &str, path: &str) -> Result<(u64, u64, String), (FErr, String)> {
        let place = parse(path).map_err(denied)?;
        check(&self.caller(who), &place, Access::Write).map_err(denied)?;
        let (sv, root, mut parts) = self.base(&place)?;
        let name = parts.pop().ok_or((FErr::AccessDenied, String::from("a mount can't be made or removed")))?;
        let mut dir = root;
        for p in parts {
            let e: f::Entry = self.ffs(f::LOOKUP, &f::Name { sv, dir, name: p })?;
            if e.kind != f::DIR {
                return Err((FErr::WrongType, String::from("a file isn't a directory")));
            }
            dir = e.ino;
        }
        Ok((sv, dir, name))
    }

    fn handle(&mut self, conn: &mut Conn, ordinal: u32, p: &[u8]) -> Reply {
        fn arg<T: fwire::Wire>(p: &[u8]) -> Result<T, (FErr, String)> {
            fwire::from_payload(p).map_err(|e| (FErr::InvalidArgs, String::from(e.describe())))
        }
        let unit = || Ok(fwire::to_payload(&()));
        let who = conn.who.clone();
        match ordinal {
            v::OPEN => {
                let o: v::Open = arg(p)?;
                let write = o.write || o.create || o.truncate;
                let access = if write { Access::Write } else { Access::Read };
                let (sv, ino) = match self.resolve(&who, &o.path, access) {
                    Ok((_, _, k)) if k == f::DIR => {
                        return Err((FErr::WrongType, String::from("a directory can't be opened")));
                    }
                    Ok((sv, ino, _)) => (sv, ino),
                    Err((FErr::NotFound, _)) if o.create => {
                        let (sv, dir, name) = self.parent(&who, &o.path)?;
                        let e: f::Entry = self.ffs(f::CREATE, &f::Create { sv, dir, name, kind: f::FILE })?;
                        (sv, e.ino)
                    }
                    Err(e) => return Err(e),
                };
                if o.truncate {
                    self.ffs::<_, ()>(f::TRUNCATE, &f::Truncate { sv, ino, size: 0 })?;
                }
                let st: f::Stat = self.ffs(f::STAT, &f::Ino { sv, ino })?;
                let id = conn.next;
                conn.next += 1;
                conn.files.push(Open { id, sv, ino, at: 0, write });
                Ok(fwire::to_payload(&v::Opened { handle: id, size: st.size }))
            }
            v::READ => {
                let r: v::ReadAt = arg(p)?;
                let o = conn.files.iter_mut().find(|o| o.id == r.handle).ok_or(denied(FErr::BadHandle))?;
                let (sv, ino, at) = (o.sv, o.ino, o.at);
                let d: f::Data = self.ffs(f::READ, &f::Read { sv, ino, offset: at, len: r.len.min(60 * 1024) })?;
                if let Some(o) = conn.files.iter_mut().find(|o| o.id == r.handle) {
                    o.at += d.bytes.len() as u64;
                }
                Ok(fwire::to_payload(&v::Data { bytes: d.bytes }))
            }
            v::WRITE => {
                let w: v::WriteAt = arg(p)?;
                let o = conn.files.iter().find(|o| o.id == w.handle).ok_or(denied(FErr::BadHandle))?;
                if !o.write {
                    return Err((FErr::AccessDenied, String::from("opened to read")));
                }
                let (sv, ino, at, n) = (o.sv, o.ino, o.at, w.bytes.len() as u64);
                self.ffs::<_, ()>(f::WRITE, &f::Write { sv, ino, offset: at, bytes: w.bytes })?;
                if let Some(o) = conn.files.iter_mut().find(|o| o.id == w.handle) {
                    o.at += n;
                }
                unit()
            }
            v::SEEK => {
                let s: v::Seek = arg(p)?;
                let o = conn.files.iter_mut().find(|o| o.id == s.handle).ok_or(denied(FErr::BadHandle))?;
                o.at = s.to;
                unit()
            }
            v::CLOSE => {
                let h: v::Handle = arg(p)?;
                conn.files.retain(|o| o.id != h.handle);
                unit()
            }
            v::STAT => {
                let path: v::Path = arg(p)?;
                let place = parse(&path.path).map_err(denied)?;
                if matches!(place, Place::Root | Place::Homes) {
                    check(&self.caller(&who), &place, Access::Read).map_err(denied)?;
                    return Ok(fwire::to_payload(&v::Stat { dir: true, size: 2, modified_ns: 0, created_ns: 0 }));
                }
                let (sv, ino, _) = self.resolve(&who, &path.path, Access::Read)?;
                let s: f::Stat = self.ffs(f::STAT, &f::Ino { sv, ino })?;
                Ok(fwire::to_payload(&v::Stat {
                    dir: s.kind == f::DIR,
                    size: s.size,
                    modified_ns: s.modified_ns,
                    created_ns: s.created_ns,
                }))
            }
            v::READDIR => {
                let path: v::Path = arg(p)?;
                let place = parse(&path.path).map_err(denied)?;
                let entries = match place {
                    Place::Root => alloc::vec![(String::from("home"), true), (String::from("system"), true)],
                    Place::Homes => homes_listed(&self.caller(&who)).into_iter().map(|u| (u, true)).collect(),
                    _ => {
                        let (sv, ino, kind) = self.resolve(&who, &path.path, Access::Read)?;
                        if kind != f::DIR {
                            return Err((FErr::WrongType, String::from("a file isn't a directory")));
                        }
                        let es: f::Entries = self.ffs(f::READDIR, &f::Ino { sv, ino })?;
                        let mut out: Vec<(String, bool)> =
                            es.entries.into_iter().map(|(n, e)| (n, e.kind == f::DIR)).collect();
                        out.sort();
                        out
                    }
                };
                Ok(fwire::to_payload(&v::Entries { entries }))
            }
            v::MKDIR => {
                let path: v::Path = arg(p)?;
                let (sv, dir, name) = self.parent(&who, &path.path)?;
                self.ffs::<_, f::Entry>(f::CREATE, &f::Create { sv, dir, name, kind: f::DIR })?;
                unit()
            }
            v::UNLINK => {
                let path: v::Path = arg(p)?;
                let (sv, dir, name) = self.parent(&who, &path.path)?;
                self.ffs::<_, ()>(f::UNLINK, &f::Name { sv, dir, name })?;
                unit()
            }
            v::RENAME => {
                let r: v::Rename = arg(p)?;
                let (sv, dir, name) = self.parent(&who, &r.from)?;
                let (to_sv, to_dir, to_name) = self.parent(&who, &r.to)?;
                if sv != to_sv {
                    return Err((
                        FErr::NotSupported,
                        String::from("a move between subvolumes is a copy (the copy engine)"),
                    ));
                }
                self.ffs::<_, ()>(f::RENAME, &f::Rename { sv, dir, name, to_dir, to_name })?;
                unit()
            }
            v::FSYNC => {
                self.ffs::<_, ()>(f::FSYNC, &())?;
                unit()
            }
            v::SNAPSHOT => {
                let s: v::Snapshot = arg(p)?;
                let (user, sv) = self.session.clone().ok_or(denied(FErr::AccessDenied))?;
                if who == "accounts" {
                    return Err(denied(FErr::AccessDenied));
                }
                let name = alloc::format!("snapshot:{user}:{}", s.name);
                self.ffs::<_, f::Subvol>(f::SNAPSHOT, &f::Snapshot { sv, name })?;
                unit()
            }
            v::WHOAMI => {
                Ok(fwire::to_payload(&v::Who { user: self.session.as_ref().map(|s| s.0.clone()).unwrap_or_default() }))
            }
            v::SET_SESSION => {
                let s: v::Session = arg(p)?;
                let allowed = who == "accounts" || (cfg!(feature = "flavour-e2e") && who == "conductor");
                if !allowed {
                    return Err((FErr::AccessDenied, String::from("only accounts sets the session")));
                }
                self.session = (!s.user.is_empty()).then_some((s.user.clone(), s.sv));
                fstd::info!("vfs: session {}", if s.user.is_empty() { "ended" } else { s.user.as_str() });
                unit()
            }
            _ => Err((FErr::NotSupported, String::from("vfs doesn't serve that"))),
        }
    }
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    let ffs = match fstd::service::connect_within("ffs", Duration::from_secs(30)) {
        Ok(ch) => Client::new(ch),
        // No storage on this machine (ffs said why): nothing to serve.
        Err(_) => {
            fstd::info!("vfs: no storage: no files to serve");
            return 0;
        }
    };
    let mut vfs = Vfs { ffs, session: None };
    let Ok((listen, give)) = Channel::pair() else { return 1 };
    if fstd::service::register(give).is_err() {
        return 1;
    }
    fstd::info!("vfs: / with /system and /home");
    let mut conns: Vec<Conn> = Vec::new();
    loop {
        let mut items = alloc::vec![[listen.0.raw(), signal::READABLE]];
        items.extend(conns.iter().map(|c| [c.ch.0.raw(), signal::READABLE | signal::PEER_CLOSED]));
        match fstd::sys::wait_many(&items, u64::MAX) {
            Ok(0) => {
                if let Ok(m) = listen.recv(0)
                    && let Ok((h, p)) = fwire::parse(&m.bytes)
                    && h.ordinal == c::INCOMING
                    && let Ok(inc) = fwire::from_payload::<c::Incoming>(p)
                    && let Some(ch) = m.handles.into_iter().next()
                {
                    conns.push(Conn { who: inc.client, ch: Channel(ch), files: Vec::new(), next: 1 });
                }
            }
            Ok(k) => {
                if !serve(&mut vfs, &mut conns[k - 1]) {
                    conns.remove(k - 1);
                }
            }
            Err(_) => fstd::time::sleep(Duration::from_millis(10)),
        }
    }
}

/// One connection's requests until its channel is empty; false when it's gone.
fn serve(vfs: &mut Vfs, conn: &mut Conn) -> bool {
    loop {
        let m = match conn.ch.try_recv() {
            Ok(m) => m,
            Err(FErr::ShouldWait) => return true,
            Err(_) => return false,
        };
        if fstd::inventory::seen(&conn.ch, &m.bytes) {
            continue;
        }
        let Ok((h, p)) = fwire::parse(&m.bytes) else { continue };
        let msg = match vfs.handle(conn, h.ordinal, p) {
            Ok(body) => fwire::message_bytes(h.txid, h.ordinal, fwire::flags::REPLY, body),
            Err((e, text)) => fwire::error_reply(h.txid, h.ordinal, e as i32, &text),
        };
        if let Ok(msg) = msg {
            let _ = conn.ch.send(&msg, Vec::new());
        }
    }
}
