// SPDX-License-Identifier: Apache-2.0
//! An account carried from one VM to another (TASK-106), as a shared disk
//! would carry it: the bundle its owner exported (`/home/<user>/Exports/
//! <user>.facct`) taken off VM A's disk and put in `/system/imports` on VM
//! B's, before B first boots (its FREALLY_DATA formatted here, as ffs would).
//!
//! e2e images only: there a home's key is derived from its username
//! (accounts' `master_key`), which is the one way the host can open it. The
//! bundle itself stays sealed with the account's password the whole way.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use svc_ffs::inode::{FileKind, ROOT_INO};
use svc_ffs::store::Store;
use svc_ffs::subvol::SYSTEM;
use svc_ffs::volume::Volume;
use svc_ffs::{BLOCK, FIRST_FREE, FsError};

/// FREALLY_DATA in a GPT disk image, read and written in place.
struct Disk {
    file: File,
    base: u64,
    blocks: u64,
}

impl Store for Disk {
    fn blocks(&self) -> u64 {
        self.blocks
    }
    fn read(&mut self, block: u64, buf: &mut [u8; BLOCK]) -> Result<(), FsError> {
        self.file.seek(SeekFrom::Start(self.base + block * BLOCK as u64)).map_err(|_| FsError::Io)?;
        self.file.read_exact(buf).map_err(|_| FsError::Io)
    }
    fn write(&mut self, block: u64, buf: &[u8; BLOCK]) -> Result<(), FsError> {
        self.file.seek(SeekFrom::Start(self.base + block * BLOCK as u64)).map_err(|_| FsError::Io)?;
        self.file.write_all(buf).map_err(|_| FsError::Io)
    }
    fn flush(&mut self) -> Result<(), FsError> {
        self.file.sync_all().map_err(|_| FsError::Io)
    }
}

fn data(path: &Path) -> Result<Disk, String> {
    let mut file =
        OpenOptions::new().read(true).write(true).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let mut head = vec![0u8; 34 * 512];
    file.read_exact(&mut head).map_err(|e| format!("{}: {e}", path.display()))?;
    let h =
        fgpt::Header::parse(&head[512..1024], 1, len / 512 - 1).map_err(|e| format!("{}: {e:?}", path.display()))?;
    let t = fgpt::Table::new(h, &head[1024..]).map_err(|e| format!("{}: {e:?}", path.display()))?;
    let p = t
        .partitions()
        .find(|p| p.type_guid == fgpt::PART_TYPE_FREALLY_DATA)
        .ok_or(format!("{}: no FREALLY_DATA partition", path.display()))?;
    Ok(Disk { file, base: p.first_lba * 512, blocks: p.blocks() * 512 / BLOCK as u64 })
}

/// A FreallyFS refusal, said with the disk and what was being done.
fn said(disk: &Path, what: &str, e: FsError) -> String {
    format!("{}: {what}: {e:?}", disk.display())
}

fn e2e_key(user: &str) -> [u8; 32] {
    fcrypto::sha2::sha256(format!("freally e2e master key: {user}").as_bytes())
}

/// The bundle `user` exported on the VM whose disk is `from`.
pub fn take(from: &Path, user: &str) -> Result<Vec<u8>, String> {
    let mut v = Volume::mount(data(from)?).map_err(|e| said(from, "mounting", e))?;
    let home = format!("home:{user}");
    let sv = v
        .subvols()
        .map_err(|e| said(from, "its subvolumes", e))?
        .into_iter()
        .find(|(_, r)| r.name == home)
        .map(|(id, _)| id)
        .ok_or(format!("{}: no {home} (run @accounts.move_out first)", from.display()))?;
    v.unlock(sv, &e2e_key(user)).map_err(|e| said(from, "unlocking the home (an e2e image's?)", e))?;
    let exports = v
        .lookup(sv, ROOT_INO, "Exports")
        .map_err(|e| said(from, "Exports", e))?
        .ok_or(format!("{}: {user} has exported nothing", from.display()))?
        .child;
    let name = format!("{user}.facct");
    let f = v
        .lookup(sv, exports, &name)
        .map_err(|e| said(from, &name, e))?
        .ok_or(format!("{}: no Exports/{name}", from.display()))?
        .child;
    let n = v.stat(sv, f).map_err(|e| said(from, &name, e))?.size;
    v.read(sv, f, 0, n).map_err(|e| said(from, &name, e))
}

/// `bundle` put in `/system/imports/<user>.facct` on the disk `to` (its
/// FREALLY_DATA formatted first if it's never been written).
pub fn put(to: &Path, user: &str, bundle: &[u8]) -> Result<(), String> {
    let mut disk = data(to)?;
    let mut fresh = true;
    let mut b = [0u8; BLOCK];
    for i in 0..FIRST_FREE {
        disk.read(i, &mut b).map_err(|e| said(to, "reading", e))?;
        fresh &= b.iter().all(|&x| x == 0);
    }
    let mut v = if fresh {
        let uuid: [u8; 16] =
            fcrypto::sha2::sha256(to.display().to_string().as_bytes())[..16].try_into().unwrap_or_default();
        Volume::format(disk, uuid).map_err(|e| said(to, "formatting", e))?
    } else {
        Volume::mount(disk).map_err(|e| said(to, "mounting", e))?
    };
    let imports = match v.lookup(SYSTEM, ROOT_INO, "imports").map_err(|e| said(to, "imports", e))? {
        Some(e) => e.child,
        None => v.create(SYSTEM, ROOT_INO, "imports", FileKind::Dir).map_err(|e| said(to, "imports", e))?,
    };
    let name = format!("{user}.facct");
    if v.lookup(SYSTEM, imports, &name).map_err(|e| said(to, &name, e))?.is_some() {
        v.unlink(SYSTEM, imports, &name).map_err(|e| said(to, &name, e))?;
    }
    let f = v.create(SYSTEM, imports, &name, FileKind::File).map_err(|e| said(to, &name, e))?;
    v.write(SYSTEM, f, 0, bundle).map_err(|e| said(to, &name, e))?;
    v.commit().map_err(|e| said(to, "committing", e))
}

/// The account carried: taken from `from`, put on `to`. The bundle's size.
pub fn carry(from: &Path, user: &str, to: &Path) -> Result<usize, String> {
    let bundle = take(from, user)?;
    put(to, user, &bundle)?;
    println!("carry: {user}'s bundle ({} bytes) from {} to {}", bundle.len(), from.display(), to.display());
    Ok(bundle.len())
}
