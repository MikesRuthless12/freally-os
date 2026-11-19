// SPDX-License-Identifier: Apache-2.0
//! Export and import through ffs (TASK-103): a home walked into a `.facct`
//! (its `Exports` folder left out), and a `.facct` read through twice, once
//! to check every chunk and the archive whole, then again to make its files.

use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::FErr;
use fstd::ipc::Client;
use fstd::time::{Duration, Instant};
use fwire::proto::ffs as f;
use svc_accounts::AccountRecord;
use svc_accounts::facct::{self, Entry, Event, Kind, Parser, Problem, Reader, Writer};

/// FreallyFS's system subvolume and every tree's root folder (ffs-format § 6).
pub const SYSTEM: u64 = 256;
pub const ROOT: u64 = 256;
/// Exports and the files waiting to be imported.
pub const EXPORTS: &str = "Exports";
pub const IMPORTS: &str = "imports";
/// The most a read asks ffs for (a reply fits in a message).
const PIECE: u64 = 48 * 1024;

pub type Failed = (FErr, String);

fn call<Req: fwire::Wire, Rep: fwire::Wire>(ffs: &mut Client, ordinal: u32, req: &Req) -> Result<Rep, Failed> {
    ffs.call::<_, Rep>(ordinal, req, Vec::new(), Instant::after(Duration::from_secs(15))).map(|(r, _)| r).map_err(|e| {
        match e {
            fstd::ipc::CallError::Remote(code, text) => (FErr::from_code(code as i64), text),
            other => (FErr::Internal, alloc::format!("ffs: {other:?}")),
        }
    })
}

fn problem(p: Problem) -> Failed {
    let code = match p {
        Problem::WrongPassword => FErr::AccessDenied,
        Problem::Kdf => FErr::NoMemory,
        Problem::Damaged | Problem::NotAnAccount => FErr::Integrity,
    };
    (code, String::from(p.words()))
}

pub fn lookup(ffs: &mut Client, sv: u64, dir: u64, name: &str) -> Result<Option<f::Entry>, Failed> {
    match call::<_, f::Entry>(ffs, f::LOOKUP, &f::Name { sv, dir, name: String::from(name) }) {
        Ok(e) => Ok(Some(e)),
        Err((FErr::NotFound, _)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The folder `name` in `dir`, made if it isn't there.
fn folder(ffs: &mut Client, sv: u64, dir: u64, name: &str) -> Result<u64, Failed> {
    if let Some(e) = lookup(ffs, sv, dir, name)? {
        return Ok(e.ino);
    }
    let made: f::Entry = call(ffs, f::CREATE, &f::Create { sv, dir, name: String::from(name), kind: f::DIR })?;
    Ok(made.ino)
}

/// Everything under folder `dir` of subvolume `sv`, folders before what's
/// in them, each with its inode: `skip` (a top-level name) left out.
fn walk(
    ffs: &mut Client,
    sv: u64,
    dir: u64,
    prefix: &str,
    skip: &str,
    out: &mut Vec<(Entry, u64)>,
) -> Result<(), Failed> {
    let list: f::Entries = call(ffs, f::READDIR, &f::Ino { sv, ino: dir })?;
    let mut entries = list.entries;
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, e) in entries {
        if prefix.is_empty() && name == skip {
            continue;
        }
        let path = if prefix.is_empty() { name.clone() } else { alloc::format!("{prefix}/{name}") };
        let st: f::Stat = call(ffs, f::STAT, &f::Ino { sv, ino: e.ino })?;
        let kind = if e.kind == f::DIR { Kind::Folder } else { Kind::File };
        let len = if kind == Kind::File { st.size } else { 0 };
        out.push((Entry { kind, path: path.clone(), modified_ns: st.modified_ns, len }, e.ino));
        if kind == Kind::Folder {
            walk(ffs, sv, e.ino, &path, skip, out)?;
        }
    }
    Ok(())
}

/// Bytes `[offset, offset + len)` of a file.
fn read(ffs: &mut Client, sv: u64, ino: u64, offset: u64, len: u64) -> Result<Vec<u8>, Failed> {
    let d: f::Data = call(ffs, f::READ, &f::Read { sv, ino, offset, len: len as u32 })?;
    if d.bytes.len() as u64 != len {
        return Err((FErr::Integrity, String::from("a file changed while it was read")));
    }
    Ok(d.bytes)
}

fn write(ffs: &mut Client, sv: u64, ino: u64, offset: u64, bytes: &[u8]) -> Result<(), Failed> {
    call::<_, ()>(ffs, f::WRITE, &f::Write { sv, ino, offset, bytes: bytes.to_vec() })
}

/// The home of subvolume `sv` (unlocked), as `<username>.facct` in its
/// `Exports` folder: the file's inode and size. `salt` and `prefix` are
/// fresh random bytes.
#[allow(clippy::too_many_arguments)]
pub fn export(
    ffs: &mut Client,
    sv: u64,
    record: &AccountRecord,
    password: &str,
    kdf: fcrypto::argon2::Params,
    salt: [u8; 16],
    prefix: [u8; 16],
    now_unix: u64,
) -> Result<u64, Failed> {
    let mut entries = Vec::new();
    walk(ffs, sv, ROOT, "", EXPORTS, &mut entries)?;
    let plain: Vec<Entry> = entries.iter().map(|(e, _)| e.clone()).collect();
    let (header, mut w) = Writer::start(password, kdf, salt, prefix, facct::fpk_len(&plain)).map_err(problem)?;
    let dir = folder(ffs, sv, ROOT, EXPORTS)?;
    let name = alloc::format!("{}.facct", record.username);
    let ino = match lookup(ffs, sv, dir, &name)? {
        Some(e) => {
            call::<_, ()>(ffs, f::TRUNCATE, &f::Truncate { sv, ino: e.ino, size: 0 })?;
            e.ino
        }
        None => call::<_, f::Entry>(ffs, f::CREATE, &f::Create { sv, dir, name, kind: f::FILE })?.ino,
    };
    let mut at = 0u64;
    let mut out = header.to_vec();
    let mut flush = |ffs: &mut Client, out: &mut Vec<u8>, all: bool| -> Result<(), Failed> {
        while out.len() as u64 >= PIECE || (all && !out.is_empty()) {
            let n = (out.len() as u64).min(PIECE) as usize;
            write(ffs, sv, ino, at, &out[..n])?;
            at += n as u64;
            out.drain(..n);
        }
        Ok(())
    };
    w.push(&facct::fpk_start(record, now_unix), &mut out).map_err(problem)?;
    for (e, src) in &entries {
        w.push(&facct::fpk_entry(e), &mut out).map_err(problem)?;
        let mut done = 0;
        while done < e.len {
            let n = (e.len - done).min(PIECE);
            let bytes = read(ffs, sv, *src, done, n)?;
            w.push(&bytes, &mut out).map_err(problem)?;
            done += n;
            flush(ffs, &mut out, false)?;
        }
        flush(ffs, &mut out, false)?;
    }
    w.push(&[facct::FPK_END], &mut out).map_err(problem)?;
    w.finish(&mut out).map_err(problem)?;
    flush(ffs, &mut out, true)?;
    call::<_, ()>(ffs, f::FSYNC, &())?;
    Ok(at)
}

/// Where a bundle's name points: (subvolume, its folder, the file's name).
pub fn place(ffs: &mut Client, name: &str, home: Option<u64>) -> Result<(u64, u64, String), Failed> {
    let missing = || (FErr::NotFound, alloc::format!("There's no account file called {name}."));
    let (sv, folder_name, file) = match name.split_once(':') {
        Some(("system", file)) => (SYSTEM, IMPORTS, file),
        Some(("home", file)) => (home.ok_or_else(missing)?, EXPORTS, file),
        _ => return Err(missing()),
    };
    if !file.ends_with(".facct") || file.contains('/') {
        return Err(missing());
    }
    let dir = lookup(ffs, sv, ROOT, folder_name)?.ok_or_else(missing)?.ino;
    Ok((sv, dir, String::from(file)))
}

/// The bundles that can be imported: `system:` ones, and the session's own.
pub fn bundles(ffs: &mut Client, home: Option<u64>) -> Result<Vec<String>, Failed> {
    let mut out = Vec::new();
    for (sv, folder_name, tag) in [(Some(SYSTEM), IMPORTS, "system"), (home, EXPORTS, "home")] {
        let Some(sv) = sv else { continue };
        let Some(dir) = lookup(ffs, sv, ROOT, folder_name)? else { continue };
        let list: f::Entries = call(ffs, f::READDIR, &f::Ino { sv, ino: dir.ino })?;
        out.extend(
            list.entries
                .into_iter()
                .filter(|(n, e)| e.kind == f::FILE && n.ends_with(".facct"))
                .map(|(n, _)| alloc::format!("{tag}:{n}")),
        );
    }
    out.sort();
    Ok(out)
}

/// Reads bundle `name` through, every chunk opened and the archive parsed,
/// each event handed to `each`; `Damaged` unless all of it is whole.
pub fn read_through(
    ffs: &mut Client,
    name: &str,
    home: Option<u64>,
    password: &str,
    mut each: impl FnMut(&mut Client, Event) -> Result<(), Failed>,
) -> Result<(), Failed> {
    let (sv, dir, file) = place(ffs, name, home)?;
    let ino = lookup(ffs, sv, dir, &file)?.ok_or((FErr::NotFound, String::from("That account file is gone.")))?.ino;
    let size = call::<_, f::Stat>(ffs, f::STAT, &f::Ino { sv, ino })?.size;
    if size < facct::HEADER as u64 {
        return Err(problem(Problem::Damaged));
    }
    let header = read(ffs, sv, ino, 0, facct::HEADER as u64)?;
    let mut r = Reader::open(&header, password).map_err(problem)?;
    if facct::size(r.len) != size {
        return Err(problem(Problem::Damaged));
    }
    let mut at = facct::HEADER as u64;
    let mut p = Parser::default();
    while !r.done() {
        let n = r.next_size() as u64;
        let mut sealed = Vec::with_capacity(n as usize);
        let mut got = 0;
        while got < n {
            let k = (n - got).min(PIECE);
            sealed.extend(read(ffs, sv, ino, at + got, k)?);
            got += k;
        }
        at += n;
        let plain = r.chunk(&sealed).map_err(problem)?;
        for e in p.feed(&plain).map_err(problem)? {
            each(ffs, e)?;
        }
    }
    if !p.done() {
        return Err(problem(Problem::Damaged));
    }
    Ok(())
}

/// What a bundle holds, its whole file checked.
pub struct Found {
    pub record: AccountRecord,
    pub exported_unix: u64,
    pub files: u64,
    pub bytes: u64,
}

pub fn preview(ffs: &mut Client, name: &str, home: Option<u64>, password: &str) -> Result<Found, Failed> {
    let mut found: Option<Found> = None;
    let (mut files, mut bytes) = (0u64, 0u64);
    read_through(ffs, name, home, password, |_, e| {
        match e {
            Event::Account { record, exported_unix } => {
                found = Some(Found { record, exported_unix, files: 0, bytes: 0 });
            }
            Event::Start(e) if e.kind == Kind::File => {
                files += 1;
                bytes += e.len;
            }
            _ => {}
        }
        Ok(())
    })?;
    let mut f = found.ok_or_else(|| problem(Problem::Damaged))?;
    (f.files, f.bytes) = (files, bytes);
    Ok(f)
}

/// A checked bundle's tree, made in the (unlocked) home `sv`.
pub fn unpack(ffs: &mut Client, name: &str, home: Option<u64>, password: &str, sv: u64) -> Result<(), Failed> {
    // Each folder's inode by path, and the file being written.
    let mut folders: Vec<(String, u64)> = Vec::new();
    let mut file: Option<(u64, u64)> = None;
    read_through(ffs, name, home, password, |ffs, e| {
        match e {
            Event::Start(e) => {
                let (parent, leaf) = match e.path.rsplit_once('/') {
                    Some((p, l)) => {
                        (folders.iter().find(|(f, _)| f == p).map(|(_, i)| *i).ok_or(problem(Problem::Damaged))?, l)
                    }
                    None => (ROOT, e.path.as_str()),
                };
                let kind = if e.kind == Kind::Folder { f::DIR } else { f::FILE };
                let made: f::Entry =
                    call(ffs, f::CREATE, &f::Create { sv, dir: parent, name: String::from(leaf), kind })?;
                if e.kind == Kind::Folder {
                    folders.push((e.path, made.ino));
                    file = None;
                } else {
                    file = Some((made.ino, 0));
                }
            }
            Event::Data(bytes) => {
                let (ino, at) = file.as_mut().ok_or(problem(Problem::Damaged))?;
                for piece in bytes.chunks(PIECE as usize) {
                    write(ffs, sv, *ino, *at, piece)?;
                    *at += piece.len() as u64;
                }
            }
            Event::Account { .. } | Event::End => {}
        }
        Ok(())
    })?;
    call::<_, ()>(ffs, f::FSYNC, &())
}
