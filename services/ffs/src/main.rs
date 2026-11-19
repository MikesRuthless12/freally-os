// SPDX-License-Identifier: Apache-2.0
//! ffs, the service (TASK-100): FreallyFS on the disk's FREALLY_DATA
//! partition, through drv-nvme's block protocol. It mounts the newest whole
//! generation (and runs `ffsck` when the last session didn't unmount),
//! formats the partition only when it's never been written (its first
//! 64 KiB all zeros), and serves FreallyFS's protocol to vfs and accounts.
//! A transaction commits on `fsync`, 5 s after its first change, or at
//! 4,096 dirty blocks.
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::{FErr, signal};
use fstd::ipc::{Channel, Client};
use fstd::time::{Duration, Instant};
use fwire::proto::block as blk;
use fwire::proto::conductor as c;
use fwire::proto::ffs as f;
use svc_ffs::inode::FileKind;
use svc_ffs::store::Store;
use svc_ffs::volume::Volume;
use svc_ffs::{BLOCK, FsError};

fstd::program!(run);

const PARTITION: &str = "FREALLY_DATA";
/// No disk to keep files on (a VM on the files layout): said, not an error.
const NO_DISK: &str = "no NVMe drive: no storage";

/// The data partition, through drv-nvme.
struct BlockStore {
    disk: Client,
    blocks: u64,
}

impl BlockStore {
    fn call<Req: fwire::Wire, Rep: fwire::Wire>(&mut self, ordinal: u32, req: &Req) -> Result<Rep, FsError> {
        self.disk
            .call::<_, Rep>(ordinal, req, Vec::new(), Instant::after(Duration::from_secs(10)))
            .map(|(r, _)| r)
            .map_err(|e| {
                fstd::error!("ffs: the disk: {e:?}");
                FsError::Io
            })
    }
}

impl Store for BlockStore {
    fn blocks(&self) -> u64 {
        self.blocks
    }

    fn read(&mut self, block: u64, buf: &mut [u8; BLOCK]) -> Result<(), FsError> {
        let d: blk::Data = self.call(blk::READ, &blk::Read { partition: String::from(PARTITION), block, count: 1 })?;
        if d.bytes.len() != BLOCK {
            return Err(FsError::Io);
        }
        buf.copy_from_slice(&d.bytes);
        Ok(())
    }

    fn write(&mut self, block: u64, buf: &[u8; BLOCK]) -> Result<(), FsError> {
        self.call::<_, ()>(blk::WRITE, &blk::Write { partition: String::from(PARTITION), block, bytes: buf.to_vec() })
    }

    /// One message for the run (drv-nvme takes up to 15 blocks a write).
    fn write_run(&mut self, first: u64, bufs: &[&[u8; BLOCK]]) -> Result<(), FsError> {
        let mut bytes = Vec::with_capacity(bufs.len() * BLOCK);
        for b in bufs {
            bytes.extend_from_slice(&b[..]);
        }
        self.call::<_, ()>(blk::WRITE, &blk::Write { partition: String::from(PARTITION), block: first, bytes })
    }

    fn flush(&mut self) -> Result<(), FsError> {
        self.call::<_, ()>(blk::FLUSH, &())
    }
}

/// The volume on FREALLY_DATA: mounted, or made on a partition never written.
fn open() -> Result<Volume<BlockStore>, String> {
    let ch = fstd::service::connect_within("drv-nvme", Duration::from_secs(10)).map_err(|_| String::from(NO_DISK))?;
    let mut disk = Client::new(ch);
    let (info, _) = disk
        .call::<_, blk::Info>(blk::INFO, &(), Vec::new(), Instant::after(Duration::from_secs(5)))
        .map_err(|e| alloc::format!("the disk's partitions: {e:?}"))?;
    let part =
        info.partitions.iter().find(|p| p.name == PARTITION && p.freally).ok_or("the disk has no FREALLY_DATA")?;
    let mut store = BlockStore { disk, blocks: part.blocks };
    let mut fresh = true;
    let mut b = [0u8; BLOCK];
    for i in 0..svc_ffs::FIRST_FREE {
        store.read(i, &mut b).map_err(|e| alloc::format!("reading the partition: {e:?}"))?;
        fresh &= b.iter().all(|&x| x == 0);
    }
    if fresh {
        let mut uuid = [0u8; 16];
        frandom::fill(&mut uuid)
            .map_err(|_| String::from("no randomness for the volume's id (RDRAND/RDSEED refused)"))?;
        let v = Volume::format(store, uuid).map_err(|e| alloc::format!("formatting: {e:?}"))?;
        fstd::info!("ffs: formatted FREALLY_DATA ({} MiB) on {}", part.blocks * 4 / 1024, info.model);
        return Ok(v);
    }
    match Volume::mount_checked(store) {
        Ok((v, None)) => {
            fstd::info!(
                "ffs: mounted generation {} ({} MiB, {} MiB free)",
                v.generation(),
                part.blocks * 4 / 1024,
                v.free_blocks() * 4 / 1024
            );
            Ok(v)
        }
        Ok((v, Some(r))) => {
            fstd::warn!("ffs: generation {} wasn't unmounted: ffsck ran", v.generation());
            for x in &r.findings {
                fstd::error!("ffs: ffsck: {}", x.what);
            }
            if r.is_clean() {
                fstd::info!("ffs: ffsck: clean ({} nodes, {} data blocks)", r.nodes, r.data);
            }
            Ok(v)
        }
        Err(FsError::NotFreallyFs) => {
            Err(String::from("FREALLY_DATA holds something that isn't FreallyFS: left as it is"))
        }
        Err(e) => Err(alloc::format!("mounting: {e:?}")),
    }
}

/// A FreallyFS error as the protocol carries it.
fn ferr(e: FsError) -> (FErr, String) {
    let code = match e {
        FsError::NotFound => FErr::NotFound,
        FsError::Exists => FErr::AlreadyExists,
        FsError::BadName | FsError::TooLarge => FErr::InvalidArgs,
        FsError::NotEmpty => FErr::Busy,
        FsError::NotADirectory | FsError::IsADirectory => FErr::WrongType,
        FsError::ReadOnly | FsError::Locked | FsError::WrongKey => FErr::AccessDenied,
        FsError::Integrity(_) | FsError::Corrupt(_) => FErr::Integrity,
        FsError::NoSpace => FErr::NoMemory,
        FsError::Io | FsError::NotFreallyFs => FErr::Internal,
    };
    (code, alloc::format!("{e:?}"))
}

fn kind_of(k: FileKind) -> u8 {
    match k {
        FileKind::File => f::FILE,
        FileKind::Dir => f::DIR,
    }
}

/// Who may use FreallyFS directly: vfs, accounts, and in e2e images
/// conductor's test door (which makes and unlocks a test home).
fn trusted(who: &str) -> bool {
    who == "vfs" || who == "accounts" || (cfg!(feature = "flavour-e2e") && who == "conductor")
}

/// One request: the reply's payload, or the error.
fn handle(v: &mut Volume<BlockStore>, ordinal: u32, p: &[u8]) -> Result<Vec<u8>, (FErr, String)> {
    fn arg<T: fwire::Wire>(p: &[u8]) -> Result<T, (FErr, String)> {
        fwire::from_payload(p).map_err(|e| (FErr::InvalidArgs, String::from(e.describe())))
    }
    let ok = |r: Result<(), FsError>| r.map(|_| fwire::to_payload(&())).map_err(ferr);
    v.set_time(fstd::time::utc_ns());
    match ordinal {
        f::LOOKUP => {
            let n: f::Name = arg(p)?;
            let e =
                v.lookup(n.sv, n.dir, &n.name).map_err(ferr)?.ok_or((FErr::NotFound, String::from("no such name")))?;
            Ok(fwire::to_payload(&f::Entry { ino: e.child, kind: kind_of(e.kind) }))
        }
        f::STAT => {
            let i: f::Ino = arg(p)?;
            let s = v.stat(i.sv, i.ino).map_err(ferr)?;
            Ok(fwire::to_payload(&f::Stat {
                kind: kind_of(s.kind),
                size: s.size,
                nlink: s.nlink,
                modified_ns: s.modified_ns,
                created_ns: s.created_ns,
            }))
        }
        f::READDIR => {
            let i: f::Ino = arg(p)?;
            let entries = v.read_dir(i.sv, i.ino).map_err(ferr)?;
            Ok(fwire::to_payload(&f::Entries {
                entries: entries
                    .into_iter()
                    .map(|e| (e.name, f::Entry { ino: e.child, kind: kind_of(e.kind) }))
                    .collect(),
            }))
        }
        f::CREATE => {
            let c: f::Create = arg(p)?;
            let k = if c.kind == f::DIR { FileKind::Dir } else { FileKind::File };
            let ino = v.create(c.sv, c.dir, &c.name, k).map_err(ferr)?;
            Ok(fwire::to_payload(&f::Entry { ino, kind: c.kind }))
        }
        f::READ => {
            let r: f::Read = arg(p)?;
            let len = r.len.min(60 * 1024) as u64;
            Ok(fwire::to_payload(&f::Data { bytes: v.read(r.sv, r.ino, r.offset, len).map_err(ferr)? }))
        }
        f::WRITE => {
            let w: f::Write = arg(p)?;
            ok(v.write(w.sv, w.ino, w.offset, &w.bytes))
        }
        f::TRUNCATE => {
            let t: f::Truncate = arg(p)?;
            ok(v.truncate(t.sv, t.ino, t.size))
        }
        f::UNLINK => {
            let n: f::Name = arg(p)?;
            ok(v.unlink(n.sv, n.dir, &n.name))
        }
        f::RENAME => {
            let r: f::Rename = arg(p)?;
            ok(v.rename(r.sv, r.dir, &r.name, r.to_dir, &r.to_name))
        }
        f::CLONE => {
            let c: f::CloneFile = arg(p)?;
            let ino = v.clone_file(c.sv, c.src, c.dir, &c.name).map_err(ferr)?;
            Ok(fwire::to_payload(&f::Entry { ino, kind: f::FILE }))
        }
        f::SET_MODIFIED => {
            let t: f::Times = arg(p)?;
            ok(v.set_modified(t.sv, t.ino, t.modified_ns))
        }
        f::FSYNC => ok(v.fsync()),
        f::SNAPSHOT => {
            let s: f::Snapshot = arg(p)?;
            Ok(fwire::to_payload(&f::Subvol { sv: v.snapshot(s.sv, &s.name).map_err(ferr)? }))
        }
        f::FIND => {
            let n: f::Named = arg(p)?;
            let found = v.subvols().map_err(ferr)?.into_iter().find(|(_, r)| r.name == n.name);
            let (sv, _) = found.ok_or((FErr::NotFound, alloc::format!("no subvolume is called {}", n.name)))?;
            Ok(fwire::to_payload(&f::Subvol { sv }))
        }
        f::CREATE_HOME => {
            let mut h: f::Home = arg(p)?;
            if v.subvols().map_err(ferr)?.iter().any(|(_, r)| r.name == h.name) {
                fstd::secret::wipe(&mut h.key);
                return Err((FErr::AlreadyExists, alloc::format!("{} is there already", h.name)));
            }
            let made = v.create_encrypted_subvol(&h.name, h.owner, &h.key);
            fstd::secret::wipe(&mut h.key);
            let sv = made.map_err(ferr)?;
            v.commit().map_err(ferr)?;
            fstd::info!("ffs: made {} (subvolume {sv}, sealed)", h.name);
            Ok(fwire::to_payload(&f::Subvol { sv }))
        }
        f::UNLOCK => {
            let mut u: f::Unlock = arg(p)?;
            let unlocked = v.unlock(u.sv, &u.key);
            fstd::secret::wipe(&mut u.key);
            ok(unlocked)
        }
        f::LOCK => {
            let s: f::Subvol = arg(p)?;
            ok(v.lock(s.sv))
        }
        f::RETIRE => {
            let r: f::Subvol = arg(p)?;
            v.retire_subvol(r.sv).map_err(ferr)?;
            v.commit().map_err(ferr)?;
            fstd::info!("ffs: subvolume {} retired (its account was removed)", r.sv);
            Ok(fwire::to_payload(&()))
        }
        f::UNMOUNT => {
            v.commit_clean().map_err(ferr)?;
            fstd::info!("ffs: unmounted cleanly at generation {}", v.generation());
            Ok(fwire::to_payload(&()))
        }
        _ => Err((FErr::NotSupported, String::from("ffs doesn't serve that"))),
    }
}

/// The crash test's load (TASK-099): what the mount found, said; then a
/// transaction after another until the machine is reset. It never returns.
#[cfg(feature = "crash-load")]
fn crash_load(v: &mut Volume<BlockStore>) {
    let found = v.load_open();
    let said = svc_ffs::load::said(&found);
    let mut l = match found {
        Ok(l) => {
            fstd::info!("ffs: crash-load: {said}");
            l
        }
        Err(_) => {
            fstd::error!("ffs: crash-load: {said}");
            loop {
                fstd::time::sleep(Duration::from_secs(3600));
            }
        }
    };
    fstd::info!("ffs: crash-load: running");
    loop {
        if let Err(e) = v.load_step(&mut l) {
            fstd::error!("ffs: crash-load: BROKEN: transaction {}: {e:?}", l.version + 1);
            loop {
                fstd::time::sleep(Duration::from_secs(3600));
            }
        }
        if l.version % 100 == 0 {
            fstd::info!("ffs: crash-load: at version {}", l.version);
        }
    }
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    let mut v = match open() {
        Ok(v) => v,
        Err(e) if e == NO_DISK => {
            fstd::info!("ffs: {e}");
            return 0;
        }
        Err(e) => {
            fstd::error!("ffs: {e}");
            return 1;
        }
    };
    // The crash test's image (TASK-099): the load, and nothing else.
    #[cfg(feature = "crash-load")]
    crash_load(&mut v);
    let Ok((listen, give)) = Channel::pair() else { return 1 };
    if fstd::service::register(give).is_err() {
        return 1;
    }
    let mut clients: Vec<(String, Channel)> = Vec::new();
    loop {
        let mut items = alloc::vec![[listen.0.raw(), signal::READABLE]];
        items.extend(clients.iter().map(|(_, ch)| [ch.0.raw(), signal::READABLE | signal::PEER_CLOSED]));
        // Wake each second for the 5 s commit.
        match fstd::sys::wait_many(&items, Instant::after(Duration::from_secs(1))) {
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
                if !serve(&mut v, who, ch) {
                    clients.remove(k - 1);
                }
            }
            Err(FErr::TimedOut) => {}
            Err(_) => fstd::time::sleep(Duration::from_millis(10)),
        }
        if v.should_commit(Instant::now().as_ns())
            && let Err(e) = v.commit()
        {
            fstd::error!("ffs: a commit failed: {e:?}");
        }
    }
}

/// One client's requests until its channel is empty; false when it's gone.
fn serve(v: &mut Volume<BlockStore>, who: &str, ch: &Channel) -> bool {
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
        // conductor's clean unmount before Shut down or Restart, in every image.
        let reply = if !trusted(who) && !(who == "conductor" && ordinal == f::UNMOUNT) {
            Err((FErr::AccessDenied, String::from("files go through vfs")))
        } else {
            handle(v, ordinal, p)
        };
        let msg = match reply {
            Ok(body) => fwire::message_bytes(txid, ordinal, fwire::flags::REPLY, body),
            Err((e, text)) => fwire::error_reply(txid, ordinal, e as i32, &text),
        };
        if let Ok(msg) = msg {
            let _ = ch.send(&msg, Vec::new());
        }
        // A home's key came in (or was just forgotten): the request is wiped,
        // and so is the stack the work used (FR-046).
        if matches!(ordinal, f::CREATE_HOME | f::UNLOCK | f::LOCK) {
            fstd::secret::wipe(&mut m.bytes);
            fstd::secret::scrub_stack();
        }
    }
}
