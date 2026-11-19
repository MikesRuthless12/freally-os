// SPDX-License-Identifier: Apache-2.0
//! Files (TASK-100): the file protocol to vfs, for any process. Paths are
//! absolute (`/home/<user>/...`, `/system/...`); vfs checks each request
//! against the caller and the session's account. Data travels in pieces
//! of 60 KiB (a message holds 64 KiB).

use crate::ipc::{CallError, Client};
use crate::time::{Duration, Instant};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use fcore_abi::FErr;
use fwire::proto::vfs as v;

/// The most bytes one message carries.
const PIECE: usize = 60 * 1024;

struct Global(RefCell<Option<Client>>);
// SAFETY: services are single-threaded until fstd has a lock (as service.rs).
unsafe impl Sync for Global {}
static VFS: Global = Global(RefCell::new(None));

/// What went wrong: the code, and vfs's (or ffs's) words for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsError {
    pub code: FErr,
    pub text: String,
}

impl FsError {
    /// In a few words, for a person: "not yours to reach (AccessDenied)".
    pub fn describe(&self) -> String {
        alloc::format!("{} ({:?})", self.text, self.code)
    }
}

impl From<CallError> for FsError {
    fn from(e: CallError) -> FsError {
        match e {
            CallError::Remote(code, text) => FsError { code: FErr::from_code(code as i64), text },
            CallError::Sys(code) => FsError { code, text: String::from("vfs didn't answer") },
            CallError::Wire(_) => {
                FsError { code: FErr::InvalidArgs, text: String::from("an answer vfs couldn't have sent") }
            }
        }
    }
}

fn call<Req: fwire::Wire, Rep: fwire::Wire>(ordinal: u32, req: &Req) -> Result<Rep, FsError> {
    let mut g = VFS.0.borrow_mut();
    if g.is_none() {
        let ch = crate::service::connect_within("vfs", Duration::from_secs(5))?;
        *g = Some(Client::new(ch));
    }
    let c = g.as_mut().ok_or(FsError { code: FErr::BadState, text: String::from("no vfs") })?;
    Ok(c.call::<_, Rep>(ordinal, req, Vec::new(), Instant::after(Duration::from_secs(15)))?.0)
}

/// An open file; closed when dropped.
pub struct File {
    handle: u32,
    pub size: u64,
}

impl File {
    /// Opens `path` to read.
    pub fn open(path: &str) -> Result<File, FsError> {
        Self::with(path, false, false, false)
    }

    /// Opens `path` to write, made if it's missing and emptied if it's there.
    pub fn create(path: &str) -> Result<File, FsError> {
        Self::with(path, true, true, true)
    }

    fn with(path: &str, write: bool, create: bool, truncate: bool) -> Result<File, FsError> {
        let o: v::Opened = call(v::OPEN, &v::Open { path: String::from(path), write, create, truncate })?;
        Ok(File { handle: o.handle, size: o.size })
    }

    /// Up to `len` bytes from where the file is (fewer at its end).
    pub fn read(&mut self, len: usize) -> Result<Vec<u8>, FsError> {
        let mut out = Vec::new();
        while out.len() < len {
            let want = (len - out.len()).min(PIECE) as u32;
            let d: v::Data = call(v::READ, &v::ReadAt { handle: self.handle, len: want })?;
            if d.bytes.is_empty() {
                break;
            }
            out.extend_from_slice(&d.bytes);
        }
        Ok(out)
    }

    pub fn write(&mut self, bytes: &[u8]) -> Result<(), FsError> {
        for piece in bytes.chunks(PIECE) {
            call::<_, ()>(v::WRITE, &v::WriteAt { handle: self.handle, bytes: piece.to_vec() })?;
        }
        Ok(())
    }

    pub fn seek(&mut self, to: u64) -> Result<(), FsError> {
        call(v::SEEK, &v::Seek { handle: self.handle, to })
    }

    /// Everything written so far is on the disk when this returns.
    pub fn sync(&mut self) -> Result<(), FsError> {
        call(v::FSYNC, &())
    }
}

impl Drop for File {
    fn drop(&mut self) {
        let _ = call::<_, ()>(v::CLOSE, &v::Handle { handle: self.handle });
    }
}

/// A whole file.
pub fn read(path: &str) -> Result<Vec<u8>, FsError> {
    let mut f = File::open(path)?;
    let n = f.size as usize;
    f.read(n)
}

/// A whole file, written (made or replaced) and synced.
pub fn write(path: &str, bytes: &[u8]) -> Result<(), FsError> {
    let mut f = File::create(path)?;
    f.write(bytes)?;
    f.sync()
}

/// What's at `path`.
pub fn metadata(path: &str) -> Result<v::Stat, FsError> {
    call(v::STAT, &v::Path { path: String::from(path) })
}

/// A directory's entries: each name and whether it's a directory.
pub fn read_dir(path: &str) -> Result<Vec<(String, bool)>, FsError> {
    Ok(call::<_, v::Entries>(v::READDIR, &v::Path { path: String::from(path) })?.entries)
}

pub fn create_dir(path: &str) -> Result<(), FsError> {
    call(v::MKDIR, &v::Path { path: String::from(path) })
}

/// Removes a file, or an empty directory.
pub fn remove(path: &str) -> Result<(), FsError> {
    call(v::UNLINK, &v::Path { path: String::from(path) })
}

pub fn rename(from: &str, to: &str) -> Result<(), FsError> {
    call(v::RENAME, &v::Rename { from: String::from(from), to: String::from(to) })
}

/// A read-only snapshot of the session's home, called `name`.
pub fn snapshot(name: &str) -> Result<(), FsError> {
    call(v::SNAPSHOT, &v::Snapshot { name: String::from(name) })
}

/// The session's account (empty: none).
pub fn whoami() -> Result<String, FsError> {
    Ok(call::<_, v::Who>(v::WHOAMI, &())?.user)
}
