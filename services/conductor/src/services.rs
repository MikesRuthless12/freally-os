// SPDX-License-Identifier: Apache-2.0
//! Supervision and the directory (TASK-038).
//!
//! - Boot services start in dependency order (`fsvc::check_set`).
//! - A service that ends is restarted per its manifest, with backoff: at once,
//!   then 100 ms, 400 ms, 1.6 s, 6.4 s; a sixth crash within 60 s of the
//!   others holds it stopped (the owner's policy, 2026-11-17).
//! - Each service's handle 1 is its directory channel: `connect`, `register`,
//!   `list_services`, `boot_timeline`, `faults`.

use crate::faults::Kept;
use crate::image::Image;
use crate::loader::{self, Child};
use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::{FErr, rights, signal, topic, well_known};
use fstd::handle::Handle;
use fstd::ipc::Channel;
use fstd::time::Instant;
use fsvc::{Manifest, Restart};
use fwire::HandleRef;
use fwire::proto::conductor::{self as c, ServiceState};

const BACKOFF_MS: [u64; 5] = [0, 100, 400, 1_600, 6_400];
const CRASH_WINDOW_NS: u64 = 60_000_000_000;
const HOLD_AFTER: usize = 5;

pub struct Service {
    pub m: Manifest,
    pub state: ServiceState,
    pub child: Option<Child>,
    /// The other end of the endpoint the service registered.
    pub endpoint: Option<Channel>,
    crashes: Vec<u64>,
    pub restarts: u32,
    pub started_ns: u64,
    start_at: Option<u64>,
    sha: [u8; 32],
    /// When the test door killed it (e2e), to report how long the restart took.
    pub killed_at: Option<u64>,
}

pub struct Conductor {
    pub image: Image,
    pub services: Vec<Service>,
    pub faults: Kept,
    /// TSC stamps conductor makes (S1, S2), raw.
    pub s1: u64,
    pub s2: u64,
    /// Lines for the test door to send (e2e): the restart reports.
    pub door_out: Vec<String>,
}

fn rdtsc() -> u64 {
    // SAFETY: RDTSC is allowed in user mode (CR4.TSD is clear).
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// What the backoff says after a crash at `now`, given the recent `crashes`:
/// `None` to hold the service stopped, else the delay before it restarts.
pub fn backoff(crashes: &mut Vec<u64>, now: u64) -> Option<u64> {
    crashes.retain(|&t| now.saturating_sub(t) < CRASH_WINDOW_NS);
    crashes.push(now);
    if crashes.len() > HOLD_AFTER {
        return None;
    }
    Some(BACKOFF_MS[(crashes.len() - 1).min(BACKOFF_MS.len() - 1)] * 1_000_000)
}

impl Conductor {
    pub fn new(image: Image, manifests: Vec<Manifest>) -> Conductor {
        let services = manifests
            .into_iter()
            .map(|m| Service {
                m,
                state: ServiceState::Waiting,
                child: None,
                endpoint: None,
                crashes: Vec::new(),
                restarts: 0,
                started_ns: 0,
                start_at: None,
                sha: [0; 32],
                killed_at: None,
            })
            .collect();
        Conductor { image, services, faults: Kept::new(), s1: 0, s2: 0, door_out: Vec::new() }
    }

    /// Queues every boot service, in dependency order.
    pub fn start_boot(&mut self, order: &[String]) {
        let now = Instant::now().as_ns();
        for name in order {
            if let Some(s) = self.services.iter_mut().find(|s| &s.m.name == name)
                && s.m.start == fsvc::Start::Boot
            {
                s.start_at = Some(now);
            }
        }
        self.start_due();
    }

    /// The handles a manifest's grants carry, by name.
    fn granted_handles(&self, m: &Manifest) -> Vec<(String, Handle)> {
        let mut out = Vec::new();
        for g in &m.grants {
            let h = match g.as_str() {
                "devices" => fstd::sys::call1(
                    fcore_abi::syscall::HANDLE_DUPLICATE,
                    [
                        well_known::ROOT_RESOURCE as u64,
                        (rights::MANAGE | rights::DUPLICATE | rights::TRANSFER) as u64,
                        0,
                        0,
                        0,
                        0,
                    ],
                )
                .ok()
                .and_then(|v| Handle::from_raw(v as u32)),
                // COM1, for the log's mirror: never in a release image.
                "serial" if self.image.flavour != 0 => {
                    fstd::sys::resource_create(well_known::ROOT_RESOURCE, fcore_abi::resource::IO_PORT, 0x3F8, 8)
                        .ok()
                        .and_then(Handle::from_raw)
                }
                _ => None,
            };
            if let Some(h) = h {
                out.push((g.clone(), h));
            }
        }
        out
    }

    /// Starts every service whose time has come.
    pub fn start_due(&mut self) {
        let now = Instant::now().as_ns();
        for i in 0..self.services.len() {
            if self.services[i].start_at.is_none_or(|t| t > now) {
                continue;
            }
            self.services[i].start_at = None;
            let m = self.services[i].m.clone();
            let grants = m.grants.iter().map(|g| loader::kernel_grant(g)).fold(0, |a, b| a | b);
            let handles = self.granted_handles(&m);
            let result = self.image.file(&m.binary).and_then(|elf| {
                self.services[i].sha = fcrypto::sha2::sha256(elf);
                loader::spawn(elf, &m.name, grants, self.image.flavour, handles)
            });
            let s = &mut self.services[i];
            match result {
                Ok(child) => {
                    s.started_ns = Instant::now().as_ns();
                    s.state = ServiceState::Starting;
                    if s.restarts > 0 {
                        fstd::info!("conductor: {} restarted (restart {}, pid {})", m.name, s.restarts, child.pid);
                    }
                    s.child = Some(child);
                }
                Err(e) => {
                    fstd::error!("conductor: can't start {}: {e}", m.name);
                    s.state = ServiceState::Held;
                }
            }
        }
    }

    /// The earliest pending start, if any.
    pub fn next_start(&self) -> Option<u64> {
        self.services.iter().filter_map(|s| s.start_at).min()
    }

    /// A service's process ended.
    pub fn exited(&mut self, i: usize) {
        let now = Instant::now().as_ns();
        let s = &mut self.services[i];
        let Some(child) = s.child.take() else { return };
        let (_, _, code) = fstd::sys::process_info(child.process.raw()).unwrap_or((0, true, -1));
        s.endpoint = None;
        let restart = match s.m.restart {
            Restart::Always => true,
            Restart::OnFailure => code != 0,
            Restart::Never => false,
        };
        if !restart {
            fstd::info!("conductor: {} ended ({code})", s.m.name);
            s.state = ServiceState::Stopped;
            return;
        }
        match backoff(&mut s.crashes, now) {
            Some(delay) => {
                fstd::warn!("conductor: {} ended ({code}); restarting in {} ms", s.m.name, delay / 1_000_000);
                s.state = ServiceState::Restarting;
                s.restarts += 1;
                s.start_at = Some(now + delay);
            }
            None => {
                fstd::error!("conductor: {} crashed {} times in 60 s; held stopped", s.m.name, HOLD_AFTER + 1);
                s.state = ServiceState::Held;
            }
        }
    }

    /// Serves one message from service `i`'s directory channel.
    pub fn request(&mut self, i: usize) {
        let Some(child) = self.services[i].child.as_ref() else { return };
        let mut bytes = alloc::vec![0u8; 1024];
        let mut hs = [0u32; 16];
        let (n, nh) = match fstd::sys::channel_read(child.channel.0.raw(), &mut bytes, &mut hs) {
            Ok(r) => r,
            Err(_) => return,
        };
        let mut handles: Vec<Option<Handle>> = hs[..nh].iter().map(|&v| Handle::from_raw(v)).collect();
        let Ok((h, payload)) = fwire::parse(&bytes[..n]) else {
            fstd::warn!("conductor: a malformed message from {}", self.services[i].m.name);
            return;
        };
        let caller = self.services[i].m.name.clone();
        let reply: Result<(Vec<u8>, Vec<Handle>), (FErr, String)> = match h.ordinal {
            c::CONNECT => self.connect(&caller, payload),
            c::REGISTER => self.register(i, payload, &mut handles),
            c::LIST_SERVICES => self.list().map(|b| (b, Vec::new())),
            c::BOOT_TIMELINE => self.timeline(h.txid).map(|b| (b, Vec::new())),
            c::FAULTS => self.faults_page(payload).map(|b| (b, Vec::new())),
            _ => Err((FErr::NotSupported, String::from("conductor doesn't serve that"))),
        };
        let msg = match reply {
            Ok((body, hs)) => fwire::message_bytes(h.txid, h.ordinal, fwire::flags::REPLY, body).map(|m| (m, hs)),
            Err((e, text)) => fwire::error_reply(h.txid, h.ordinal, e as i32, &text).map(|m| (m, Vec::new())),
        };
        if let (Ok((m, hs)), Some(child)) = (msg, self.services[i].child.as_ref()) {
            let _ = child.channel.send(&m, hs);
        }
    }

    fn connect(&mut self, caller: &str, payload: &[u8]) -> Result<(Vec<u8>, Vec<Handle>), (FErr, String)> {
        let want: c::ByName =
            fwire::from_payload(payload).map_err(|e| (FErr::InvalidArgs, String::from(e.describe())))?;
        let caller_m = self.services.iter().find(|s| s.m.name == caller).map(|s| s.m.clone());
        let allowed =
            want.name == "logger" || want.name == "stage" || caller_m.is_some_and(|m| m.after.contains(&want.name));
        if !allowed {
            return Err((FErr::AccessDenied, alloc::format!("{caller} may not connect to {}", want.name)));
        }
        let target = self
            .services
            .iter()
            .find(|s| s.m.name == want.name)
            .ok_or((FErr::NotFound, alloc::format!("no service is called {}", want.name)))?;
        let endpoint =
            target.endpoint.as_ref().ok_or((FErr::ShouldWait, alloc::format!("{} isn't ready yet", want.name)))?;
        let (a, b) = Channel::pair().map_err(|e| (e, String::from("channel_create")))?;
        let incoming = fwire::event(c::INCOMING, &c::Incoming { client: String::from(caller), channel: HandleRef(0) })
            .map_err(|e| (FErr::Internal, String::from(e.describe())))?;
        endpoint.send(&incoming, alloc::vec![a.0]).map_err(|e| (e, String::from("the service's endpoint")))?;
        Ok((fwire::to_payload(&c::Connected { channel: HandleRef(0) }), alloc::vec![b.0]))
    }

    fn register(
        &mut self,
        i: usize,
        payload: &[u8],
        handles: &mut [Option<Handle>],
    ) -> Result<(Vec<u8>, Vec<Handle>), (FErr, String)> {
        let r: c::Register =
            fwire::from_payload(payload).map_err(|e| (FErr::InvalidArgs, String::from(e.describe())))?;
        if r.name != self.services[i].m.name {
            return Err((FErr::AccessDenied, String::from("a service registers only its own name")));
        }
        let ep = handles
            .get_mut(r.endpoint.0 as usize)
            .and_then(Option::take)
            .ok_or((FErr::InvalidArgs, String::from("no endpoint")))?;
        let now = Instant::now().as_ns();
        let s = &mut self.services[i];
        s.endpoint = Some(Channel(ep));
        s.state = ServiceState::Running;
        if let Some(t) = s.killed_at.take() {
            let ms = (now.saturating_sub(t)) / 1_000_000;
            fstd::info!("conductor: {} back in {ms} ms", s.m.name);
            self.door_out.push(alloc::format!("back {} {ms}", s.m.name));
        }
        if r.name == "logger" {
            self.attach_log();
        }
        self.check_s1();
        Ok((Vec::new(), Vec::new()))
    }

    /// Conductor's own lines go to the logger once it's up.
    fn attach_log(&mut self) {
        if let Some(ch) = self.open_connection("logger", "conductor") {
            fstd::log::attach(ch);
            fstd::info!("conductor: logger ready");
        }
    }

    /// S1: the logger (and devmgr, once it exists) registered.
    fn check_s1(&mut self) {
        if self.s1 != 0 {
            return;
        }
        let ready = |n: &str| self.services.iter().find(|s| s.m.name == n).is_none_or(|s| s.endpoint.is_some());
        if ready("logger") && ready("devmgr") {
            self.s1 = rdtsc();
        }
    }

    /// A new client channel to the registered service `to`, as if `client` had connected.
    pub fn open_connection(&self, to: &str, client: &str) -> Option<Channel> {
        let ep = self.services.iter().find(|s| s.m.name == to)?.endpoint.as_ref()?;
        let (a, b) = Channel::pair().ok()?;
        let incoming =
            fwire::event(c::INCOMING, &c::Incoming { client: String::from(client), channel: HandleRef(0) }).ok()?;
        ep.send(&incoming, alloc::vec![a.0]).ok()?;
        Some(b)
    }

    fn list(&self) -> Result<Vec<u8>, (FErr, String)> {
        let services = self
            .services
            .iter()
            .map(|s| c::ServiceInfo {
                name: s.m.name.clone(),
                description: s.m.description.clone(),
                state: s.state,
                pid: s.child.as_ref().map_or(0, |c| c.pid),
                restarts: s.restarts,
                started_ns: s.started_ns,
                binary_sha256: s.sha,
            })
            .collect();
        Ok(fwire::to_payload(&c::Services { services }))
    }

    /// Every stage so far, in ns since B0.
    pub fn stages(&self) -> Vec<(u8, u64)> {
        let mut cpu = [0u8; 16];
        let hz = fstd::sys::system_info(topic::CPU, &mut cpu)
            .ok()
            .map(|_| u64::from_le_bytes(cpu[8..16].try_into().unwrap_or_default()))
            .unwrap_or(0);
        let mut raw = alloc::vec![0u8; 15 * 9];
        let n = fstd::sys::system_info(topic::BOOT_TIMELINE, &mut raw).unwrap_or(0).min(raw.len());
        let mut stamps: Vec<(u8, u64)> = raw[..n]
            .as_chunks::<9>()
            .0
            .iter()
            .map(|c| (c[0], u64::from_le_bytes(c[1..9].try_into().unwrap_or_default())))
            .collect();
        for (id, t) in [(11u8, self.s1), (12u8, self.s2)] {
            if t != 0 {
                stamps.push((id, t));
            }
        }
        let b0 = stamps.iter().find(|(id, _)| *id == 2).map_or(0, |s| s.1);
        if hz == 0 {
            return Vec::new();
        }
        stamps
            .into_iter()
            .map(|(id, t)| (id, ((t.saturating_sub(b0)) as u128 * 1_000_000_000 / hz as u128) as u64))
            .collect()
    }

    fn timeline(&self, _txid: u32) -> Result<Vec<u8>, (FErr, String)> {
        Ok(fwire::to_payload(&c::BootTimeline { stages: self.stages() }))
    }

    fn faults_page(&self, payload: &[u8]) -> Result<Vec<u8>, (FErr, String)> {
        let p: c::Page = fwire::from_payload(payload).map_err(|e| (FErr::InvalidArgs, String::from(e.describe())))?;
        let limit = (p.limit as usize).clamp(1, 32);
        let after: Vec<&c::FaultReport> = self.faults.reports.iter().filter(|r| r.seq > p.cursor).collect();
        let page: Vec<c::FaultReport> = after.iter().take(limit).map(|r| (*r).clone()).collect();
        let next = if after.len() > limit { page.last().map_or(0, |r| r.seq) } else { 0 };
        Ok(fwire::to_payload(&c::Faults { reports: page, next_cursor: next }))
    }

    /// What to wait for: (handle, signals) and what each means.
    pub fn wait_items(&self) -> (Vec<[u32; 2]>, Vec<Wake>) {
        let mut items = alloc::vec![[well_known::FAULTS_EVENT, crate::faults::NEW_FAULT]];
        let mut what = alloc::vec![Wake::Faults];
        for (i, s) in self.services.iter().enumerate() {
            if let Some(c) = &s.child {
                items.push([c.process.raw(), signal::TERMINATED]);
                what.push(Wake::Exited(i));
                items.push([c.channel.0.raw(), signal::READABLE]);
                what.push(Wake::Request(i));
            }
        }
        (items, what)
    }
}

#[derive(Clone, Copy)]
pub enum Wake {
    Faults,
    Exited(usize),
    Request(usize),
}

#[cfg(test)]
mod tests {
    use super::backoff;

    #[test]
    fn the_backoff_is_the_owners_policy() {
        let s = 1_000_000_000u64;
        let mut crashes = alloc::vec::Vec::new();
        let delays: alloc::vec::Vec<Option<u64>> = (0..6).map(|i| backoff(&mut crashes, i * s)).collect();
        assert_eq!(
            delays,
            [Some(0), Some(100_000_000), Some(400_000_000), Some(1_600_000_000), Some(6_400_000_000), None]
        );
        // A crash a minute after the others starts the count again.
        let mut crashes = alloc::vec![0u64, s, 2 * s, 3 * s, 4 * s];
        assert_eq!(backoff(&mut crashes, 70 * s), Some(0));
        // Six within 60 s, however spread: held.
        let mut crashes = alloc::vec![0u64, 10 * s, 20 * s, 40 * s, 50 * s];
        assert_eq!(backoff(&mut crashes, 59 * s), None);
    }
}
