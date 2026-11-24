// SPDX-License-Identifier: Apache-2.0
//! Online updates (TASK-182, FR-061; PRD § 3, the update index). Nothing is
//! fetched until Settings asks (*Check online*): then the host's index,
//! checked by the keys this system's bootloader trusts (whatever the host's
//! certificate), and what it offers this system. Nothing more until the
//! person says yes: then the package, a piece at a time, straight into the
//! install TASK-110 made (its checks unchanged), every byte counted and
//! hashed, and its size and SHA-256 held to the index's before the install
//! is finished.
//!
//! The host: a release build's is compiled in once the owner picks it (with
//! its root); until then none, and Settings says so. An e2e build takes a
//! test host and its root from the test door, never from a person.

use crate::{Reply, Updater, refused};
use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::FErr;
use fcrypto::sha2::Sha256;
use fhttps::{Stream, Url};
use fsi::index::{self, Entry};
use fstd::ipc::Client;
use fstd::time::{Duration, Instant};
use fwire::proto::updater as up;

/// A package may take this long to come whole.
const WHOLE: Duration = Duration::from_secs(30 * 60);
/// Each step writes what comes in this long, then answers Settings (its meter).
const STEP: Duration = Duration::from_millis(250);

/// Where updates come from: the host and the roots its chain must end at.
pub struct Host {
    pub name: String,
    pub roots: &'static [&'static [u8]],
}

/// This build's host: none yet (the owner picks it; § 15 Q4).
pub fn built_in() -> Option<Host> {
    None
}

/// A package on its way.
struct Get {
    stream: Stream<'static>,
    entry: Entry,
    /// Every byte so far, counted and hashed; the image's bytes written.
    got: u64,
    hash: Sha256,
    image_at: u64,
}

#[derive(Default)]
pub struct Online {
    pub host: Option<Host>,
    net: Option<Client>,
    /// The last check's offer: its package's URL and entry.
    offer: Option<(Url, Entry)>,
    get: Option<Get>,
}

fn flavour_name(f: u8) -> &'static str {
    match f {
        0 => "release",
        1 => "dev",
        _ => "e2e",
    }
}

fn failed(why: String) -> (FErr, String) {
    (FErr::BadState, why)
}

impl Online {
    pub fn new(host: Option<Host>) -> Online {
        Online { host, ..Default::default() }
    }

    fn net(&mut self) -> Result<&mut Client, (FErr, String)> {
        if self.net.is_none() {
            let ch = fstd::service::connect("netstack")
                .map_err(|_| (FErr::NotFound, String::from("There's no network to check for updates on.")))?;
            self.net = Some(Client::new(ch));
        }
        self.net.as_mut().ok_or((FErr::NotFound, String::from("There's no network.")))
    }

    /// The package being fetched, given up (its connection closed).
    pub fn drop_get(&mut self) {
        if let Some(mut g) = self.get.take()
            && let Some(net) = self.net.as_mut()
        {
            g.stream.close(net);
        }
    }
}

/// The image's bytes from `at` into the install, a piece (60 KiB) at a time,
/// as the install takes them from Settings.
fn feed(i: &mut crate::Install, at: u64, bytes: &[u8], disk: &mut crate::Disk) -> Result<(), (FErr, String)> {
    let mut at = at;
    for piece in bytes.chunks(svc_updater::PIECE) {
        i.piece(at, piece, disk).map_err(refused)?;
        at += piece.len() as u64;
    }
    Ok(())
}

impl Updater {
    pub(crate) fn online(&mut self, ordinal: u32, p: &[u8]) -> Reply {
        let r = self.online_inner(ordinal, p);
        if let Err((_, why)) = &r {
            fstd::warn!("updater: online: {why}");
        }
        r
    }

    fn online_inner(&mut self, ordinal: u32, p: &[u8]) -> Reply {
        #[cfg(not(feature = "flavour-e2e"))]
        let _ = p;
        match ordinal {
            up::ONLINE_CHECK => self.check().map(|o| fwire::to_payload(&o)),
            up::ONLINE_BEGIN => self.begin_online().map(|()| fwire::to_payload(&())),
            up::ONLINE_STEP => match self.step_online() {
                Ok(pr) => Ok(fwire::to_payload(&pr)),
                Err(e) => {
                    self.online.drop_get();
                    self.install = None;
                    Err(e)
                }
            },
            #[cfg(feature = "flavour-e2e")]
            up::TEST_HOST => self.test_host(p),
            _ => Err((FErr::NotSupported, String::from("not served"))),
        }
    }

    /// The test door's test host and its root (e2e images only).
    #[cfg(feature = "flavour-e2e")]
    fn test_host(&mut self, p: &[u8]) -> Reply {
        let t: up::TestHost = crate::arg(p)?;
        // Kept for the life of the service: a test door's few roots.
        let root: &'static [u8] = alloc::boxed::Box::leak(t.root.into_boxed_slice());
        let roots: &'static [&'static [u8]] = alloc::boxed::Box::leak(alloc::vec![root].into_boxed_slice());
        fstd::info!("updater: the test host is {}", t.host);
        self.online.host = Some(Host { name: t.host, roots });
        Ok(fwire::to_payload(&()))
    }

    /// The host's index, checked, and what it offers this system.
    fn check(&mut self) -> Result<up::Offer, (FErr, String)> {
        let keys = self.keys();
        let (name, roots) = match &self.online.host {
            Some(h) => (h.name.clone(), h.roots),
            None => return Err((FErr::NotFound, String::from("No update host is set for this build yet."))),
        };
        let dir = alloc::format!("https://{name}/{}/", flavour_name(self.flavour));
        let net = self.online.net()?;
        let got = fhttps::fetch(net, &alloc::format!("{dir}index.fui"), roots, index::MAX_LEN)
            .map_err(|e| failed(alloc::format!("The update host couldn't be reached: {e}")))?;
        let r = &got.response;
        if r.status != 200 {
            return Err(failed(alloc::format!("The update host answered {} {}.", r.status, r.reason)));
        }
        let ix = index::parse(&r.body, &keys, self.flavour).map_err(|e| failed(String::from(e.describe())))?;
        let offer = index::offer(&ix, &self.running).cloned();
        fstd::info!(
            "updater: {name} offers {} package(s){}",
            ix.entries.len(),
            offer.as_ref().map(|e| alloc::format!(", {} for this system", e.version)).unwrap_or_default()
        );
        let Some(e) = offer else {
            self.online.offer = None;
            return Err((FErr::NotFound, alloc::format!("Freally OS {} is up to date.", self.running)));
        };
        let url = Url::parse(&alloc::format!("{dir}{}", e.name)).map_err(failed)?;
        let o = up::Offer {
            version: e.version.clone(),
            oldest: e.oldest.clone(),
            notes: e.notes.clone(),
            size: e.size,
            host: name,
        };
        self.online.offer = Some((url, e));
        Ok(o)
    }

    /// The offer's package opened; its header and notes read and checked
    /// as BEGIN checks them, the slot's first block cleared.
    fn begin_online(&mut self) -> Result<(), (FErr, String)> {
        let keys = self.keys();
        let (url, entry) = self.online.offer.clone().ok_or(failed(String::from("Check online first.")))?;
        let roots = self.online.host.as_ref().map(|h| h.roots).ok_or(failed(String::from("No update host.")))?;
        self.online.drop_get();
        let net = self.online.net()?;
        let size = usize::try_from(entry.size).map_err(|_| failed(String::from("That package is too big.")))?;
        let mut stream =
            Stream::open(net, &url, roots, size, WHOLE).map_err(|e| failed(alloc::format!("The package: {e}")))?;
        if stream.status != 200 {
            stream.close(net);
            return Err(failed(alloc::format!("The update host answered {} {}.", stream.status, stream.reason)));
        }
        let (mut head, mut hash, mut got) = (Vec::new(), Sha256::new(), 0u64);
        let need = loop {
            let need = (head.len() >= fsi::fup::HEADER)
                .then(|| fsi::fup::HEADER + u32::from_le_bytes([head[12], head[13], head[14], head[15]]) as usize);
            if let Some(n) = need.filter(|&n| head.len() >= n || n > fsi::fup::HEADER + fsi::fup::MAX_NOTES) {
                break n;
            }
            match stream.next(net) {
                Ok(Some(piece)) => {
                    got += piece.len() as u64;
                    hash.update(&piece);
                    head.extend_from_slice(&piece);
                }
                Ok(None) => {
                    stream.close(net);
                    return Err(failed(String::from("The package ended before its header did.")));
                }
                Err(e) => {
                    stream.close(net);
                    return Err(failed(alloc::format!("The package: {e}")));
                }
            }
        };
        let rest = head.split_off(need.min(head.len()));
        let begun = match self.disk.as_mut() {
            None => Err((FErr::NotFound, String::from("There's no disk to update."))),
            Some(disk) => crate::Install::begin(&head, &keys, self.flavour, &self.running, disk)
                .map_err(refused)
                .and_then(|mut i| {
                    if !rest.is_empty() {
                        feed(&mut i, 0, &rest, disk)?;
                    }
                    Ok(i)
                }),
        };
        let i = match begun {
            Ok(i) => i,
            Err(e) => {
                if let Some(net) = self.online.net.as_mut() {
                    stream.close(net);
                }
                return Err(e);
            }
        };
        fstd::info!(
            "updater: downloading {} ({} bytes) into slot {}",
            i.head.version(),
            entry.size,
            crate::fbootctl_name(1 - self.slot)
        );
        self.install = Some(i);
        self.online.get = Some(Get { stream, entry, got, hash, image_at: rest.len() as u64 });
        Ok(())
    }

    /// The next part written; at the end, the package held to the index,
    /// then finished and put on trial.
    fn step_online(&mut self) -> Result<up::Progress, (FErr, String)> {
        let until = Instant::after(STEP);
        let net = self.online.net.as_mut().ok_or(failed(String::from("No package is on its way.")))?;
        let g = self.online.get.as_mut().ok_or(failed(String::from("No package is on its way.")))?;
        let (Some(i), Some(disk)) = (self.install.as_mut(), self.disk.as_mut()) else {
            return Err(failed(String::from("No update is being installed.")));
        };
        while let Some(piece) = g.stream.next(net).map_err(|e| failed(alloc::format!("The package: {e}")))? {
            g.got += piece.len() as u64;
            if g.got > g.entry.size {
                return Err(failed(String::from("The package is longer than the update list said.")));
            }
            g.hash.update(&piece);
            feed(i, g.image_at, &piece, disk)?;
            g.image_at += piece.len() as u64;
            if Instant::now().as_ns() >= until {
                return Ok(up::Progress { got: g.got, total: g.entry.size, version: String::new() });
            }
        }
        // Whole: the package the index named, or nothing is finished.
        let Some(mut g) = self.online.get.take() else { return Err(failed(String::from("No package."))) };
        g.stream.close(net);
        let hash = core::mem::replace(&mut g.hash, Sha256::new()).finish();
        if g.got != g.entry.size || !fcrypto::ct::eq(&hash, &g.entry.sha256) {
            self.install = None;
            fstd::warn!("updater: the package wasn't the one the index named ({} bytes)", g.got);
            return Err(failed(String::from(
                "The package isn't the one the update list named, so it wasn't installed.",
            )));
        }
        let version = self.finish_install()?;
        Ok(up::Progress { got: g.got, total: g.entry.size, version })
    }
}
