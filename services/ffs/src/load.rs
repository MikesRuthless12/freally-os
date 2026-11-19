// SPDX-License-Identifier: Apache-2.0
//! The crash-consistency load (TASK-099, FR-044): transactions whose result
//! can be judged after a crash at any point. Transaction `t` rewrites
//! [`FILES`] files whole at version `t` (a header naming the file and `t`,
//! then bytes made from both: 1 to 41 KiB, so files span blocks and change
//! size), removes `extra-<t-1>`, makes `extra-<t>`, and commits. After a
//! mount, [`Volume::load_check`] says whether what's there is one whole
//! transaction. It runs in a sealed subvolume, so the sealing is under test
//! too. The host tests crash it on a [`crate::store::MemStore`]; e2e images
//! built with `crash-load` run it in the OS under `cargo xtask crash-test`.

use crate::FsError;
use crate::inode::{FileKind, ROOT_INO};
use crate::store::Store;
use crate::volume::Volume;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub const FILES: u64 = 4;
/// The load's subvolume.
pub const NAME: &str = "crash-load";

/// The load's key: a test key, for a subvolume that holds only the load.
pub fn key() -> [u8; 32] {
    fcrypto::sha2::sha256(b"freally crash-load test key")
}

/// File `file`'s bytes at `version` (file [`FILES`] is the `extra-` one).
pub fn content(file: u64, version: u64) -> Vec<u8> {
    let mut x = ((file + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ version.wrapping_mul(0xBF58_476D_1CE4_E5B9)) | 1;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let len = 1024 + (next() % (40 * 1024)) as usize;
    let mut b = Vec::with_capacity(len + 8);
    b.extend_from_slice(b"FRLYLOAD");
    b.extend_from_slice(&file.to_le_bytes());
    b.extend_from_slice(&version.to_le_bytes());
    while b.len() < len {
        b.extend_from_slice(&next().to_le_bytes());
    }
    b.truncate(len);
    b
}

/// The load's place and the version it's at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Load {
    pub sv: u64,
    pub version: u64,
}

impl<S: Store> Volume<S> {
    /// The load's subvolume (made sealed and committed if it isn't there),
    /// unlocked, and checked: the load at the version found, or what's wrong.
    pub fn load_open(&mut self) -> Result<Load, String> {
        let e = |what: &str, err: FsError| format!("{what}: {err:?}");
        let found = self.subvols().map_err(|x| e("the subvolumes", x))?.into_iter().find(|(_, r)| r.name == NAME);
        let sv = match found {
            Some((sv, _)) => {
                self.unlock(sv, &key()).map_err(|x| e("unlocking the load", x))?;
                sv
            }
            None => {
                let sv = self.create_encrypted_subvol(NAME, [0; 16], &key()).map_err(|x| e("making the load", x))?;
                self.commit().map_err(|x| e("committing it", x))?;
                sv
            }
        };
        // ffsck again with the key in: the mount's ran before it, and so
        // judged only the plain trees.
        let r = self.fsck().map_err(|x| e("ffsck with the key", x))?;
        if !r.is_clean() {
            let what: Vec<&str> = r.findings.iter().map(|f| f.what.as_str()).collect();
            return Err(format!("ffsck with the key: {}", what.join("; ")));
        }
        let version = self.load_check(sv)?;
        Ok(Load { sv, version })
    }

    /// The version every file of the load is at (0 before the first
    /// transaction), or what isn't one whole transaction.
    pub fn load_check(&mut self, sv: u64) -> Result<u64, String> {
        let entries = self.read_dir(sv, ROOT_INO).map_err(|x| format!("listing the load: {x:?}"))?;
        if entries.is_empty() {
            return Ok(0);
        }
        let mut version = None;
        let mut extras = Vec::new();
        for d in &entries {
            let (file, at) = match d.name.strip_prefix("file-").map(str::parse::<u64>) {
                Some(Ok(i)) if i < FILES => (i, None),
                _ => match d.name.strip_prefix("extra-").map(str::parse::<u64>) {
                    Some(Ok(t)) => (FILES, Some(t)),
                    _ => return Err(format!("{} isn't the load's", d.name)),
                },
            };
            let size = self.stat(sv, d.child).map_err(|x| format!("{}: {x:?}", d.name))?.size;
            let bytes = self.read(sv, d.child, 0, size).map_err(|x| format!("reading {}: {x:?}", d.name))?;
            let v = bytes.get(16..24).map(|b| u64::from_le_bytes(b.try_into().unwrap_or_default())).unwrap_or(0);
            if bytes != content(file, v) {
                return Err(format!("{} isn't what version {v} wrote ({} bytes)", d.name, bytes.len()));
            }
            if let Some(t) = at {
                if t != v {
                    return Err(format!("{} holds version {v}", d.name));
                }
                extras.push(t);
                continue;
            }
            match version {
                None => version = Some(v),
                Some(w) if w != v => {
                    return Err(format!("file-{file} is at version {v}, another at {w}: half a transaction"));
                }
                Some(_) => {}
            }
        }
        let v = version.ok_or("no file-, only extras")?;
        let files = entries.iter().filter(|d| d.name.starts_with("file-")).count() as u64;
        if files != FILES || extras != [v] {
            return Err(format!("version {v}: {files} files and extras {extras:?}: half a transaction"));
        }
        Ok(v)
    }

    /// One transaction, committed.
    pub fn load_step(&mut self, l: &mut Load) -> Result<(), FsError> {
        let t = l.version + 1;
        for i in 0..FILES {
            let name = format!("file-{i}");
            let ino = match self.lookup(l.sv, ROOT_INO, &name)? {
                Some(d) => d.child,
                None => self.create(l.sv, ROOT_INO, &name, FileKind::File)?,
            };
            let bytes = content(i, t);
            self.truncate(l.sv, ino, 0)?;
            self.write(l.sv, ino, 0, &bytes)?;
        }
        if l.version > 0 {
            self.unlink(l.sv, ROOT_INO, &format!("extra-{}", l.version))?;
        }
        let extra = self.create(l.sv, ROOT_INO, &format!("extra-{t}"), FileKind::File)?;
        self.write(l.sv, extra, 0, &content(FILES, t))?;
        self.commit()?;
        l.version = t;
        Ok(())
    }
}

/// A name for what the load found, for the log.
pub fn said(found: &Result<Load, String>) -> String {
    match found {
        Ok(l) if l.version == 0 => String::from("fresh: nothing written yet"),
        Ok(l) => format!("version {} whole", l.version),
        Err(e) => format!("BROKEN: {e}"),
    }
}
