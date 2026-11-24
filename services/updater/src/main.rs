// SPDX-License-Identifier: Apache-2.0
//! updater, the service (TASK-110, FR-050): Settings ▸ Updates sends it a
//! `.fup` a piece at a time; it writes the image to the slot not running
//! (through drv-nvme, which lets it reach only the two system partitions),
//! checks it, and asks conductor to put it on trial from the next boot. It
//! trusts the keys this system's bootloader trusts, and serves Settings
//! alone (and, in an e2e image, the test door's test host). Online updates
//! (`online`, TASK-182) feed the same install from the update host.
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::{FErr, signal};
use fstd::ipc::{Channel, Client};
use fstd::time::{Duration, Instant};
use fwire::proto::block as blk;
use fwire::proto::conductor as c;
use fwire::proto::updater as up;
use svc_updater::{Install, Refusal, Slot};

/// The public keys, as the bootloader has them.
#[path = "../../../boot/src/keys.rs"]
#[allow(dead_code)]
mod keys;

mod online;

fstd::program!(run);

/// The keys this system's bootloader trusts, and their names (its rule:
/// boot/src/main.rs's `TRUSTED`).
#[cfg(feature = "flavour-release")]
fn trusted() -> Vec<([u8; 32], &'static str)> {
    alloc::vec![match keys::RELEASE {
        Some(k) => (k, "release"),
        None => (keys::STAGING, "staging"),
    }]
}
#[cfg(not(feature = "flavour-release"))]
fn trusted() -> Vec<([u8; 32], &'static str)> {
    alloc::vec![(keys::DEV, "dev"), (keys::STAGING, "staging")]
}

/// The slot not running, through drv-nvme.
struct Disk {
    cl: Client,
    partition: String,
    blocks: u64,
}

impl Disk {
    fn call<Req: fwire::Wire, Rep: fwire::Wire>(&mut self, ordinal: u32, req: &Req) -> Result<Rep, String> {
        self.cl
            // Past drv-nvme's own 30 s for a command: its answer, not ours, says it failed.
            .call::<_, Rep>(ordinal, req, Vec::new(), Instant::after(Duration::from_secs(35)))
            .map(|(r, _)| r)
            .map_err(|e| alloc::format!("the disk: {e:?}"))
    }
}

impl Slot for Disk {
    fn blocks(&self) -> u64 {
        self.blocks
    }
    fn write(&mut self, block: u64, bytes: &[u8]) -> Result<(), String> {
        let w = blk::Write { partition: self.partition.clone(), block, bytes: bytes.to_vec() };
        self.call::<_, ()>(blk::WRITE, &w)
    }
    fn read(&mut self, block: u64, count: u32) -> Result<Vec<u8>, String> {
        let r = blk::Read { partition: self.partition.clone(), block, count };
        Ok(self.call::<_, blk::Data>(blk::READ, &r)?.bytes)
    }
    fn flush(&mut self) -> Result<(), String> {
        self.call::<_, ()>(blk::FLUSH, &())
    }
}

struct Updater {
    disk: Option<Disk>,
    running: String,
    /// The slot this boot came from.
    slot: u8,
    flavour: u8,
    install: Option<Install>,
    online: online::Online,
}

type Reply = Result<Vec<u8>, (FErr, String)>;

fn refused(r: Refusal) -> (FErr, String) {
    let e = match r {
        Refusal::Disk(_) => FErr::Internal,
        Refusal::OutOfOrder | Refusal::NotAllThere => FErr::BadState,
        _ => FErr::InvalidArgs,
    };
    (e, r.describe())
}

fn ask<Req: fwire::Wire, Rep: fwire::Wire>(ordinal: u32, req: &Req) -> Result<Rep, (FErr, String)> {
    fstd::service::ask(ordinal, req).map_err(|e| match e {
        fstd::ipc::CallError::Remote(code, text) => (FErr::from_code(code as i64), text),
        e => (FErr::Internal, alloc::format!("conductor: {e:?}")),
    })
}

fn arg<T: fwire::Wire>(p: &[u8]) -> Result<T, (FErr, String)> {
    fwire::from_payload(p).map_err(|e| (FErr::InvalidArgs, String::from(e.describe())))
}

impl Updater {
    fn keys(&self) -> Vec<[u8; 32]> {
        trusted().into_iter().map(|(k, _)| k).collect()
    }

    fn handle(&mut self, ordinal: u32, p: &[u8]) -> Reply {
        let keys = self.keys();
        match ordinal {
            up::INSPECT => {
                let b: up::Bytes = arg(p)?;
                let (h, notes) = svc_updater::inspect(&b.data, &keys, self.flavour, &self.running).map_err(refused)?;
                let key = String::from(trusted()[h.key_index].1);
                let info = up::Info {
                    version: String::from(h.version()),
                    oldest: String::from(h.oldest()),
                    notes,
                    size: h.total(),
                    key,
                };
                Ok(fwire::to_payload(&info))
            }
            up::BEGIN => {
                let b: up::Bytes = arg(p)?;
                let disk = self.disk.as_mut().ok_or((FErr::NotFound, String::from("There's no disk to update.")))?;
                let i = Install::begin(&b.data, &keys, self.flavour, &self.running, disk).map_err(refused)?;
                fstd::info!("updater: installing {} into slot {}", i.head.version(), fbootctl_name(1 - self.slot));
                self.install = Some(i);
                Ok(fwire::to_payload(&()))
            }
            up::PIECE => {
                let piece: up::Piece = arg(p)?;
                let (Some(i), Some(disk)) = (self.install.as_mut(), self.disk.as_mut()) else {
                    return Err((FErr::BadState, String::from("No update is being installed.")));
                };
                if let Err(r) = i.piece(piece.at, &piece.data, disk) {
                    self.install = None;
                    return Err(refused(r));
                }
                Ok(fwire::to_payload(&()))
            }
            up::FINISH => {
                let version = self.finish_install()?;
                Ok(fwire::to_payload(&up::Version { version }))
            }
            up::ONLINE_CHECK | up::ONLINE_BEGIN | up::ONLINE_STEP | up::TEST_HOST => self.online(ordinal, p),
            up::CANCEL => {
                self.online.drop_get();
                if self.install.take().is_some() {
                    fstd::info!("updater: the install was given up");
                }
                Ok(fwire::to_payload(&()))
            }
            up::STATE => {
                let s: c::BootState = ask(c::BOOT_STATE, &())?;
                let other = (1 - self.slot) as usize;
                let o =
                    s.slots.get(other).cloned().unwrap_or(c::BootSlot { state: 0, tries: 0, version: String::new() });
                let other_state = match self.disk.as_mut() {
                    Some(d) => svc_updater::other_state(o.state, d),
                    None => o.state,
                };
                let state = up::State {
                    running: self.running.clone(),
                    slot: self.slot,
                    other_state,
                    other_version: o.version,
                    other_next: s.active as usize == other,
                };
                Ok(fwire::to_payload(&state))
            }
            up::SWITCH_BACK => {
                if self
                    .disk
                    .as_mut()
                    .is_some_and(|d| svc_updater::other_state(svc_updater::GOOD, d) == svc_updater::EMPTY)
                {
                    return Err((
                        FErr::BadState,
                        String::from("There's nothing to switch back to: the last update didn't finish installing."),
                    ));
                }
                ask::<_, ()>(c::BOOT_SWITCH, &())?;
                fstd::info!("updater: switching back at the next restart");
                Ok(fwire::to_payload(&()))
            }
            up::RESTART => {
                ask::<_, ()>(c::POWER, &c::Power { off: false })?;
                Ok(fwire::to_payload(&()))
            }
            _ => Err((FErr::NotSupported, String::from("not served"))),
        }
    }
}

impl Updater {
    /// The install checked whole, read back, and the trial asked for.
    fn finish_install(&mut self) -> Result<String, (FErr, String)> {
        let (Some(i), Some(disk)) = (self.install.take(), self.disk.as_mut()) else {
            return Err((FErr::BadState, String::from("No update is being installed.")));
        };
        let version = i.finish(disk).map_err(refused)?;
        let other = 1 - self.slot;
        ask::<_, ()>(c::BOOT_TRIAL, &c::BootTrial { slot: other, version: version.clone() })?;
        fstd::info!(
            "updater: {version} written to slot {} and checked: it starts at the next restart",
            fbootctl_name(other)
        );
        Ok(version)
    }
}

fn fbootctl_name(slot: u8) -> &'static str {
    if slot == 0 { "A" } else { "B" }
}

/// The slot not running, as drv-nvme has it.
fn disk(slot: u8) -> Result<Disk, String> {
    let ch = fstd::service::connect_within("drv-nvme", Duration::from_secs(10)).map_err(|e| alloc::format!("{e:?}"))?;
    let mut cl = Client::new(ch);
    let (info, _) = cl
        .call::<_, blk::Info>(blk::INFO, &(), Vec::new(), Instant::after(Duration::from_secs(5)))
        .map_err(|e| alloc::format!("{e:?}"))?;
    let partition = String::from(if slot == 0 { "FREALLY_SYSTEM_A" } else { "FREALLY_SYSTEM_B" });
    let p = info.partitions.iter().find(|p| p.name == partition).ok_or("no partition for the other slot")?;
    Ok(Disk { cl, blocks: p.blocks, partition })
}

fn serve(u: &mut Updater, who: &str, ch: &Channel) -> bool {
    loop {
        let m = match ch.try_recv() {
            Ok(m) => m,
            Err(FErr::ShouldWait) => return true,
            Err(_) => return false,
        };
        if fstd::inventory::seen(ch, &m.bytes) {
            continue;
        }
        let Ok((h, p)) = fwire::parse(&m.bytes) else { continue };
        // The test door's test host: an e2e image's, from conductor.
        #[cfg(feature = "flavour-e2e")]
        let door = who == "conductor" && h.ordinal == up::TEST_HOST;
        #[cfg(not(feature = "flavour-e2e"))]
        let door = false;
        let reply = if who == "settings" || door {
            u.handle(h.ordinal, p)
        } else {
            Err((FErr::AccessDenied, String::from("only Settings installs updates")))
        };
        let msg = match reply {
            Ok(body) => fwire::message_bytes(h.txid, h.ordinal, fwire::flags::REPLY, body),
            Err((e, text)) => fwire::error_reply(h.txid, h.ordinal, e as i32, &text),
        };
        if let Ok(msg) = msg {
            let _ = ch.send(&msg, Vec::new());
        }
    }
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    let Some(image) = fstd::machine::image() else {
        fstd::error!("updater: the kernel didn't say which image is running");
        return 1;
    };
    let disk = match disk(1 - image.slot) {
        Ok(d) => Some(d),
        Err(e) => {
            fstd::warn!("updater: no disk to update ({e})");
            None
        }
    };
    let mut u = Updater {
        disk,
        running: image.version,
        slot: image.slot,
        flavour: image.flavour,
        install: None,
        online: online::Online::new(online::built_in()),
    };
    let Ok((listen, give)) = Channel::pair() else { return 1 };
    if fstd::service::register(give).is_err() {
        return 1;
    }
    fstd::info!("updater: ready (running {} from slot {})", u.running, fbootctl_name(u.slot));
    let mut clients: Vec<(String, Channel)> = Vec::new();
    loop {
        let mut items = alloc::vec![[listen.0.raw(), signal::READABLE]];
        items.extend(clients.iter().map(|(_, ch)| [ch.0.raw(), signal::READABLE | signal::PEER_CLOSED]));
        match fstd::sys::wait_many(&items, Instant::after(Duration::from_secs(60))) {
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
                if !serve(&mut u, who, ch) {
                    let (who, _) = clients.remove(k - 1);
                    // Settings closed part-way: the install is given up.
                    if who == "settings" {
                        u.online.drop_get();
                        u.install = None;
                    }
                }
            }
            Err(FErr::TimedOut) => {}
            Err(_) => fstd::time::sleep(Duration::from_millis(10)),
        }
    }
}
