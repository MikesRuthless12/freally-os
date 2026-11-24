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
use alloc::rc::Rc;
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
    pub(crate) start_at: Option<u64>,
    pub(crate) sha: [u8; 32],
    /// When the test door killed it (e2e), to report how long the restart took.
    pub killed_at: Option<u64>,
    /// The build running when a swap replaced the image's (dev images, TASK-083).
    pub(crate) build: Option<Rc<[u8]>>,
    /// What a rollback restores: `Some(None)`, the image's build.
    pub(crate) before: Option<Option<Rc<[u8]>>>,
    /// Until when (ns) the swapped-in build ending rolls it back.
    pub(crate) probation: Option<u64>,
    /// The state the next start is handed (a swap's `handoff`).
    pub(crate) handoff: Option<Handle>,
}

pub struct Conductor {
    pub image: Image,
    pub services: Vec<Service>,
    pub faults: Kept,
    /// TSC stamps conductor makes (S1, S2), raw; and those stage and the
    /// desktop send (S3, S4).
    pub s1: u64,
    pub s2: u64,
    pub s3: u64,
    pub s4: u64,
    /// The image's data files (fonts, hardware bundles), each copied once
    /// into a VMO; services granted them get read-only duplicates.
    data: Vec<(String, Handle)>,
    /// Lines for the test door to send (e2e): the restart reports.
    pub door_out: Vec<String>,
    /// The A/B record (TASK-045).
    pub boot: crate::bootctl::Boot,
    /// The FPDT's reset end and OS-loader start (ns, the firmware's clock), if any.
    pub firmware: Option<(u64, u64)>,
    /// When the machine resets (the Brain's `reboot`, dev images).
    pub reboot_at: Option<u64>,
    /// Shut down (`true`) or Restart, and when (TASK-102).
    pub power_at: Option<(bool, u64)>,
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
                build: None,
                before: None,
                probation: None,
                handoff: None,
            })
            .collect();
        let boot = crate::bootctl::Boot::new(image.slot, &image.bootctl);
        Conductor {
            image,
            services,
            faults: Kept::new(),
            s1: 0,
            s2: 0,
            s3: 0,
            s4: 0,
            data: Vec::new(),
            door_out: Vec::new(),
            boot,
            firmware: None,
            reboot_at: None,
            power_at: None,
        }
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

    /// The fonts a `fonts` grant hands over at start: `/system/fonts/*.ttf`
    /// as `font:<file>`. The art (`hardware`, `os`) waits to be asked for
    /// (`DATA`): nine scales of it, a service uses one.
    fn font_vmos(&mut self) -> Vec<(String, Handle)> {
        let names: Vec<String> = (self.image.paths().unwrap_or_default().into_iter())
            .filter_map(|p| {
                p.strip_prefix("/system/fonts/").filter(|f| f.ends_with(".ttf")).map(|f| alloc::format!("font:{f}"))
            })
            .collect();
        names.into_iter().filter_map(|n| self.data_vmo(&n).map(|h| (n, h))).collect()
    }

    /// The data file called `name` (`font:<file>`, `hardware:<stem>`,
    /// `os:<stem>`), read-only: in a VMO of its own made the first time,
    /// holding the file's length (u64, little-endian) and then the file
    /// (`fstd::service::data` reads it), kept for whoever asks next.
    fn data_vmo(&mut self, name: &str) -> Option<Handle> {
        if !self.data.iter().any(|(n, _)| n == name) {
            let path = data_path(name)?;
            let bytes = self.image.file(&path).ok()?;
            let vmo = match fstd::service::sized_vmo(bytes) {
                Ok(v) => v,
                Err(e) => {
                    fstd::error!("conductor: can't hand out {path}: {}", e.describe());
                    return None;
                }
            };
            self.data.push((String::from(name), vmo));
        }
        // Read-only: the bytes, their length, and nothing to write with.
        let (_, v) = self.data.iter().find(|(n, _)| n == name)?;
        v.duplicate(rights::READ | rights::MAP | rights::TRANSFER).ok()
    }

    /// `DATA`: a data file the caller's grants allow (TASK-090A).
    fn data_file(&mut self, caller: &str, payload: &[u8]) -> Result<(Vec<u8>, Vec<Handle>), (FErr, String)> {
        let b: c::ByName = fwire::from_payload(payload).map_err(|e| (FErr::InvalidArgs, String::from(e.describe())))?;
        let grant =
            data_grant(&b.name).ok_or((FErr::InvalidArgs, alloc::format!("no data file is called {}", b.name)))?;
        let granted =
            self.services.iter().find(|s| s.m.name == caller).is_some_and(|s| s.m.grants.iter().any(|g| g == grant));
        if !granted {
            return Err((FErr::AccessDenied, alloc::format!("{caller} has no {grant} grant")));
        }
        let vmo = self.data_vmo(&b.name).ok_or((FErr::NotFound, alloc::format!("the image has no {}", b.name)))?;
        Ok((Vec::new(), alloc::vec![vmo]))
    }

    /// The handles a manifest's grants carry, by name.
    fn granted_handles(&mut self, m: &Manifest) -> Vec<(String, Handle)> {
        let mut out = Vec::new();
        if m.grants.iter().any(|x| x == "fonts") {
            out.extend(self.font_vmos());
        }
        let dup = |h: u32, r: u32| {
            fstd::sys::call1(fcore_abi::syscall::HANDLE_DUPLICATE, [h as u64, r as u64, 0, 0, 0, 0])
                .ok()
                .and_then(|v| Handle::from_raw(v as u32))
        };
        for g in &m.grants {
            // devmgr reads the ACPI root and the framebuffer from BootInfo.
            if g == "devices"
                && let Some(b) = dup(well_known::BOOTINFO_VMO, rights::READ | rights::MAP | rights::TRANSFER)
            {
                out.push((String::from("bootinfo"), b));
            }
            // `log=screen` (TASK-074): the logger draws the log on the
            // framebuffer until stage begins. A dev image only, and only when
            // the boot options ask (the bootloader reads them in dev and e2e).
            if g == "framebuffer"
                && self.image.flavour == 1
                && fcore_abi::cmdline::has(&self.image.cmdline, "log=screen")
                && self.image.framebuffer.1 > 0
            {
                let (base, size) = self.image.framebuffer;
                let fb = fstd::sys::resource_create(
                    well_known::ROOT_RESOURCE,
                    fcore_abi::resource::MMIO,
                    base,
                    size.next_multiple_of(4096),
                )
                .ok()
                .and_then(Handle::from_raw);
                if let (Some(fb), Some(b)) =
                    (fb, dup(well_known::BOOTINFO_VMO, rights::READ | rights::MAP | rights::TRANSFER))
                {
                    out.push((String::from("framebuffer"), fb));
                    out.push((String::from("bootinfo"), b));
                }
                continue;
            }
            let h = match g.as_str() {
                "devices" => dup(well_known::ROOT_RESOURCE, rights::MANAGE | rights::DUPLICATE | rights::TRANSFER),
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
            let mut handles = self.granted_handles(&m);
            if let Some(state) = self.services[i].handoff.take() {
                handles.push((String::from("handoff"), state));
            }
            // A swapped-in build (dev images), else the image's.
            let build = self.services[i].build.clone();
            let elf = match &build {
                Some(b) => Ok(&b[..]),
                None => self.image.file(&m.binary),
            };
            let result = elf.and_then(|elf| {
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
        // A swapped-in build that ends within 5 s is rolled back (FR-018),
        // and the build before it starts again at once.
        if let Some(until) = s.probation.take()
            && now < until
        {
            fstd::warn!("conductor: {}'s new build ended within 5 s ({code}); rolled back", s.m.name);
            s.build = s.before.take().flatten();
            s.state = ServiceState::Restarting;
            s.restarts += 1;
            s.start_at = Some(now);
            return;
        }
        let restart = match s.m.restart {
            Restart::Always => true,
            Restart::OnFailure => code != 0,
            Restart::Never => false,
        };
        if !restart {
            fstd::info!("conductor: {} ended ({code})", s.m.name);
            s.state = ServiceState::Stopped;
            self.check_s2();
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
        // Read whole: a message too big for a fixed buffer would stay at the
        // channel's head, and conductor would wake for it forever (closeout review).
        let raw = child.channel.0.raw();
        let Ok((bytes, hs)) = fstd::ipc::read_whole(|b, h| fstd::sys::channel_read(raw, b, h)) else { return };
        if fstd::inventory::seen_if(cfg!(feature = "flavour-e2e"), &child.channel, &bytes) {
            return;
        }
        let mut handles: Vec<Option<Handle>> = hs.iter().map(|&v| Handle::from_raw(v)).collect();
        let Ok((h, payload)) = fwire::parse(&bytes) else {
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
            c::STAMP => self.stamp(&caller, payload).map(|b| (b, Vec::new())),
            c::MANIFEST => self.manifest(payload).map(|b| (b, Vec::new())),
            c::DATA => self.data_file(&caller, payload),
            c::POWER => self.power(&caller, payload),
            c::BOOT_STATE => self.boot_state(&caller),
            c::BOOT_TRIAL => self.boot_trial(&caller, payload),
            c::BOOT_SWITCH => self.boot_switch(&caller),
            c::SET_ZONE => self.set_zone(&caller, payload),
            #[cfg(feature = "flavour-dev")]
            c::SWAP => self.swap(&caller, payload, &mut handles),
            #[cfg(feature = "flavour-dev")]
            c::ROLLBACK => self.rollback(&caller, payload),
            #[cfg(feature = "flavour-dev")]
            c::RESTART => self.restart_now(&caller, payload),
            #[cfg(feature = "flavour-dev")]
            c::REBOOT => self.reboot(&caller, payload),
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

    /// The A/B record, for the updater and Settings (TASK-110).
    fn boot_state(&mut self, caller: &str) -> Result<(Vec<u8>, Vec<Handle>), (FErr, String)> {
        if !matches!(caller, "updater" | "settings") {
            return Err((FErr::AccessDenied, alloc::format!("{caller} may not read the A/B record")));
        }
        let ctl = self.boot.ctl.ok_or((FErr::NotFound, String::from("there's no A/B record")))?;
        let slots = ctl
            .slot
            .iter()
            .map(|s| c::BootSlot { state: s.state as u8, tries: s.tries_left, version: String::from(s.version()) })
            .collect();
        Ok((fwire::to_payload(&c::BootState { slots, active: ctl.active, booted: self.boot.slot }), Vec::new()))
    }

    /// An update in the slot not running, on trial from the next boot: the
    /// updater's alone, after it wrote and checked the image (TASK-110).
    fn boot_trial(&mut self, caller: &str, payload: &[u8]) -> Result<(Vec<u8>, Vec<Handle>), (FErr, String)> {
        if caller != "updater" {
            return Err((FErr::AccessDenied, alloc::format!("{caller} may not start an update")));
        }
        let t: c::BootTrial =
            fwire::from_payload(payload).map_err(|e| (FErr::InvalidArgs, String::from(e.describe())))?;
        if t.slot > 1 || t.slot == self.boot.slot || t.version.is_empty() || t.version.len() > 8 {
            return Err((
                FErr::InvalidArgs,
                String::from("a trial goes in the other slot, its version at most 8 bytes"),
            ));
        }
        self.boot
            .start_trial(t.slot, &t.version)
            .map_err(|e| (e, String::from("the firmware refused the A/B record")))?;
        Ok((fwire::to_payload(&()), Vec::new()))
    }

    /// Switch back to the other slot's system (the updater's alone).
    fn boot_switch(&mut self, caller: &str) -> Result<(Vec<u8>, Vec<Handle>), (FErr, String)> {
        if caller != "updater" {
            return Err((FErr::AccessDenied, alloc::format!("{caller} may not switch the system")));
        }
        self.boot.switch_to_other().map_err(|e| (FErr::BadState, e))?;
        fstd::info!("conductor: the next boot is from slot {}", fbootctl::slot_name(1 - self.boot.slot));
        Ok((fwire::to_payload(&()), Vec::new()))
    }

    /// The zone (Settings ▸ Time and language alone, TASK-180A): the kernel
    /// takes it at once, then writes `FreallyClock`; a firmware that won't
    /// keep it leaves it set until the restart, and says so.
    fn set_zone(&mut self, caller: &str, payload: &[u8]) -> Result<(Vec<u8>, Vec<Handle>), (FErr, String)> {
        if caller != "settings" {
            return Err((FErr::AccessDenied, alloc::format!("{caller} may not change the zone")));
        }
        let z: c::Zone = fwire::from_payload(payload).map_err(|e| (FErr::InvalidArgs, String::from(e.describe())))?;
        let zone = fbootctl::clock::Zone { offset_min: z.offset_min, rtc_local: z.rtc_local };
        if !zone.valid() {
            return Err((
                FErr::InvalidArgs,
                String::from("a zone is a whole quarter hour from UTC\u{2212}12:00 to UTC+14:00"),
            ));
        }
        let keeps = if zone.rtc_local { "local time" } else { "UTC" };
        if let Err(e) = fstd::sys::uefi_set_var(fbootctl::clock::NAME, &zone.encode()) {
            fstd::warn!(
                "conductor: the zone is {} until the restart: the firmware didn't keep it ({e:?})",
                zone.name()
            );
            return Err((e, String::from("Set until the restart: this computer's firmware won't keep it.")));
        }
        fstd::info!("conductor: the zone is {}; the RTC keeps {keeps}", zone.name());
        Ok((fwire::to_payload(&()), Vec::new()))
    }

    /// Shut down or Restart (the login screen, the desktop): in a third of a
    /// second, so the answer leaves first.
    fn power(&mut self, caller: &str, payload: &[u8]) -> Result<(Vec<u8>, Vec<Handle>), (FErr, String)> {
        let p: c::Power = fwire::from_payload(payload).map_err(|e| (FErr::InvalidArgs, String::from(e.describe())))?;
        // The updater restarts to finish an update; it never shuts down.
        if !(matches!(caller, "login" | "desktop") || (caller == "updater" && !p.off)) {
            return Err((FErr::AccessDenied, alloc::format!("{caller} may not shut the machine down")));
        }
        fstd::info!("conductor: {} ({caller} asked)", if p.off { "shutting down" } else { "restarting" });
        self.power_at = Some((p.off, Instant::now().as_ns() + 300_000_000));
        Ok((fwire::to_payload(&()), Vec::new()))
    }

    /// The time for Shut down or Restart has come: ffs commits clean, then
    /// the machine powers off or resets. Returns only if it couldn't.
    pub fn power_now(&mut self, off: bool) {
        if let Some(ch) = self.open_connection("ffs", "conductor") {
            let mut ffs = fstd::ipc::Client::new(ch);
            let deadline = Instant::now().as_ns() + 5_000_000_000;
            if let Err(e) = ffs.call::<_, ()>(fwire::proto::ffs::UNMOUNT, &(), Vec::new(), deadline) {
                fstd::warn!("conductor: ffs didn't unmount cleanly ({e:?}): the next start checks the disk");
            }
        }
        // The last lines reach the log (COM1) before the machine goes.
        fstd::time::sleep(fstd::time::Duration::from_millis(200));
        let e = if off { fstd::sys::system_power_off() } else { fstd::sys::system_reset() };
        fstd::error!("conductor: the machine didn't {}: {}", if off { "power off" } else { "restart" }, e.describe());
    }

    fn connect(&mut self, caller: &str, payload: &[u8]) -> Result<(Vec<u8>, Vec<Handle>), (FErr, String)> {
        let want: c::ByName =
            fwire::from_payload(payload).map_err(|e| (FErr::InvalidArgs, String::from(e.describe())))?;
        let caller_m = self.services.iter().find(|s| s.m.name == caller).map(|s| s.m.clone());
        let demand = self.services.iter().any(|s| s.m.name == want.name && s.m.start == fsvc::Start::Demand);
        // The desktop starts apps: it may reach any service started on demand.
        // Every process may reach vfs, which checks each request itself
        // (the caller's name and the session's account: TASK-100).
        let allowed = want.name == "logger"
            || want.name == "stage"
            || want.name == "vfs"
            || (caller == "desktop" && demand)
            // Whether anyone is logged in, so the desktop knows which screen to start (TASK-102).
            || (caller == "desktop" && want.name == "accounts")
            // Settings ▸ Accounts, and a person's look saved (TASK-104);
            // accounts checks each request itself. Not `after`: a release
            // image has no accounts, and Settings is a guest's there.
            || (caller == "settings" && want.name == "accounts")
            // Settings ▸ Updates (TASK-110); the updater serves Settings alone.
            || (caller == "settings" && want.name == "updater")
            // The shell's ip and ping, Settings ▸ Network (TASK-174,
            // TASK-183), and the updater's online check and download, only
            // when a person asks (TASK-182); netstack checks each caller
            // itself. Not `after`: an image with no network card has no
            // netstack running.
            || (matches!(caller, "terminal" | "settings" | "updater") && want.name == "netstack")
            || caller_m.is_some_and(|m| m.after.contains(&want.name))
            || (cfg!(feature = "flavour-e2e") && caller == "cdpd");
        if !allowed {
            return Err((FErr::AccessDenied, alloc::format!("{caller} may not connect to {}", want.name)));
        }
        let target = self
            .services
            .iter()
            .find(|s| s.m.name == want.name)
            .ok_or((FErr::NotFound, alloc::format!("no service is called {}", want.name)))?;
        // A demand service not running yet starts now; the caller waits for
        // it (cdpd only ever looks: it starts nothing).
        if target.endpoint.is_none() && caller != "cdpd" && self.start_on_demand(&want.name) {
            return Err((FErr::ShouldWait, alloc::format!("{} is starting", want.name)));
        }
        let target = self.services.iter().find(|s| s.m.name == want.name).ok_or((FErr::NotFound, String::new()))?;
        // Ended for good (cleanly, or held after too many crashes): no use
        // waiting (stage, finding no PS/2 driver, carries on at once).
        if target.endpoint.is_none() && matches!(target.state, ServiceState::Stopped | ServiceState::Held) {
            return Err((FErr::BadState, alloc::format!("{} isn't running", want.name)));
        }
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
        self.services[i].endpoint = Some(Channel(ep));
        self.came_up(i);
        if r.name == "logger" {
            self.attach_log();
        }
        self.check_s1();
        self.check_s2();
        Ok((Vec::new(), Vec::new()))
    }

    /// Service `i` is up (it registered, or stamped its milestone): Running,
    /// and, after a kill, how long it took to come back.
    fn came_up(&mut self, i: usize) {
        let now = Instant::now().as_ns();
        let s = &mut self.services[i];
        s.state = ServiceState::Running;
        if let Some(t) = s.killed_at.take() {
            let ms = (now.saturating_sub(t)) / 1_000_000;
            fstd::info!("conductor: {} back in {ms} ms", s.m.name);
            self.door_out.push(alloc::format!("back {} {ms}", s.m.name));
        }
    }

    /// S2 (PRD § 3): the framebuffer and input drivers are ready, an input
    /// driver that found no hardware and ended cleanly counting as ready;
    /// conductor says the boot so far. (A trial slot's 10 s start at S4, when
    /// the desktop takes input: `stamp`.)
    pub fn check_s2(&mut self) {
        if self.s2 != 0 {
            return;
        }
        let ready = |name: &str, or_stopped: bool| {
            self.services
                .iter()
                .find(|s| s.m.name == name)
                .is_none_or(|s| s.state == ServiceState::Running || (or_stopped && s.state == ServiceState::Stopped))
        };
        if ready("drv-gop-fb", false) && ready("drv-i8042", true) {
            self.s2 = rdtsc();
            self.firmware = self.firmware_times();
            fstd::info!("conductor: boot: {}", boot_line(&self.stages()));
        }
    }

    /// Starts `name` if it's a `start = demand` service not running and not
    /// already on its way; true if it was started now.
    pub fn start_on_demand(&mut self, name: &str) -> bool {
        let Some(s) = self.services.iter_mut().find(|s| s.m.name == name) else { return false };
        if s.m.start != fsvc::Start::Demand
            || s.child.is_some()
            || s.start_at.is_some()
            || !matches!(s.state, ServiceState::Waiting | ServiceState::Stopped)
        {
            return false;
        }
        s.start_at = Some(Instant::now().as_ns());
        self.start_due();
        true
    }

    /// `stamp`: S3 from stage (its first frame on screen), S4 from the
    /// desktop (its Rack takes input); each once, from that service only.
    fn stamp(&mut self, caller: &str, payload: &[u8]) -> Result<Vec<u8>, (FErr, String)> {
        let s: c::Stamp = fwire::from_payload(payload).map_err(|e| (FErr::InvalidArgs, String::from(e.describe())))?;
        let slot = match (s.stage, caller) {
            (13, "stage") => &mut self.s3,
            (14, "desktop") => &mut self.s4,
            _ => return Err((FErr::AccessDenied, alloc::format!("{caller} may not stamp stage {}", s.stage))),
        };
        let first = *slot == 0;
        let i = self.services.iter().position(|v| v.m.name == caller);
        // A milestone says the service is up, endpoint or not (the desktop
        // serves no one). Restarted, it says so again: up, the boot's times
        // left as they were.
        match (first, i) {
            (true, _) => *slot = s.tsc,
            (false, Some(i)) if self.services[i].restarts > 0 => {
                self.came_up(i);
                return Ok(Vec::new());
            }
            _ => return Err((FErr::BadState, String::from("already stamped"))),
        }
        if let Some(i) = i {
            self.came_up(i);
        }
        fstd::info!("conductor: boot: {}", boot_line(&self.stages()));
        // The desktop takes input: a trial slot's 10 s start (PRD § 3, rule 4).
        if s.stage == 14 {
            self.boot.up();
        }
        Ok(Vec::new())
    }

    /// The firmware's own times from the ACPI FPDT (devmgr reads it), if any.
    fn firmware_times(&self) -> Option<(u64, u64)> {
        let ch = self.open_connection("devmgr", "conductor")?;
        let deadline = Instant::after(fstd::time::Duration::from_secs(2));
        let (t, _) = fstd::ipc::Client::new(ch)
            .call::<_, fwire::proto::devmgr::FirmwareTimes>(
                fwire::proto::devmgr::FIRMWARE_TIMES,
                &(),
                Vec::new(),
                deadline,
            )
            .ok()?;
        (t.reset_end != 0 || t.os_loader_start != 0).then_some((t.reset_end, t.os_loader_start))
    }

    /// Conductor's own lines go to the logger once it's up.
    fn attach_log(&mut self) {
        if let Some(ch) = self.open_connection("logger", "conductor") {
            fstd::log::attach(ch);
            fstd::info!("conductor: logger ready");
            self.boot.announce();
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
        let usage = |process: u32| fstd::sys::process_usage(process).unwrap_or((0, 0));
        // Conductor first: PID 1, running since C0, never restarted.
        let me = fcore_abi::well_known::SELF_PROCESS;
        let (cpu_ns, mem_bytes) = usage(me);
        let mut services = alloc::vec![c::ServiceInfo {
            name: String::from("conductor"),
            description: String::from(
                "Starts every service in order, restarts the ones that crash, and keeps the boot's times."
            ),
            state: c::ServiceState::Running,
            pid: fstd::sys::process_info(me).map_or(1, |(pid, ..)| pid),
            restarts: 0,
            started_ns: 0,
            binary_sha256: [0; 32],
            cpu_ns,
            mem_bytes,
        }];
        services.extend(self.services.iter().map(|s| {
            let (cpu_ns, mem_bytes) = s.child.as_ref().map_or((0, 0), |c| usage(c.process.raw()));
            c::ServiceInfo {
                name: s.m.name.clone(),
                description: s.m.description.clone(),
                state: s.state,
                pid: s.child.as_ref().map_or(0, |c| c.pid),
                restarts: s.restarts,
                started_ns: s.started_ns,
                binary_sha256: s.sha,
                cpu_ns,
                mem_bytes,
            }
        }));
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
        for (id, t) in [(11u8, self.s1), (12u8, self.s2), (13u8, self.s3), (14u8, self.s4)] {
            if t != 0 {
                stamps.push((id, t));
            }
        }
        let b0 = stamps.iter().find(|(id, _)| *id == 2).map_or(0, |s| s.1);
        if hz == 0 {
            return Vec::new();
        }
        let mut out: Vec<(u8, u64)> = stamps
            .into_iter()
            .map(|(id, t)| (id, ((t.saturating_sub(b0)) as u128 * 1_000_000_000 / hz as u128) as u64))
            .collect();
        // F0/F1: the firmware's own times (ns on its clock), from the FPDT.
        if let Some((f0, f1)) = self.firmware {
            out.insert(0, (1, f1));
            out.insert(0, (0, f0));
        }
        out
    }

    fn timeline(&self, _txid: u32) -> Result<Vec<u8>, (FErr, String)> {
        Ok(fwire::to_payload(&c::BootTimeline { stages: self.stages() }))
    }

    /// A service's manifest, as the image carries it (not secret: the
    /// manifests are in the public repo).
    fn manifest(&self, payload: &[u8]) -> Result<Vec<u8>, (FErr, String)> {
        let b: c::ByName = fwire::from_payload(payload).map_err(|e| (FErr::InvalidArgs, String::from(e.describe())))?;
        let m = self
            .services
            .iter()
            .find(|s| s.m.name == b.name)
            .ok_or((FErr::NotFound, alloc::format!("no service is called {}", b.name)))?;
        let path = alloc::format!("/system/manifests/{}.svc", m.m.name);
        let bytes = self.image.file(&path).map_err(|e| (FErr::NotFound, e))?;
        let text = core::str::from_utf8(bytes).map_err(|_| (FErr::Integrity, alloc::format!("{path} isn't UTF-8")))?;
        Ok(fwire::to_payload(&c::Text { text: String::from(text) }))
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

/// The boot so far, for the log (the specs read it until cdpd exists):
/// each stage in ms after B0, then the firmware's own times when the FPDT
/// gives them (on its own clock: shown apart, never added in).
pub fn boot_line(stages: &[(u8, u64)]) -> String {
    const NAMES: [&str; 15] = c::STAGES;
    let ms = |ns: u64| alloc::format!("{}.{}", ns / 1_000_000, ns / 100_000 % 10);
    let mut line = String::new();
    for (id, ns) in stages.iter().filter(|(id, _)| *id >= 2) {
        line.push_str(&alloc::format!("{}={} ", NAMES.get(*id as usize).unwrap_or(&"?"), ms(*ns)));
    }
    line.push_str("ms after B0; firmware ");
    match (stages.iter().find(|s| s.0 == 0), stages.iter().find(|s| s.0 == 1)) {
        (Some(f0), Some(f1)) => line.push_str(&alloc::format!("F0={} F1={} ms on its own clock", ms(f0.1), ms(f1.1))),
        _ => line.push_str("times: no FPDT"),
    }
    line
}

/// The grant a data file's name needs: `font:` fonts, `hardware:`
/// hardware, `os:` os.
pub fn data_grant(name: &str) -> Option<&'static str> {
    [("font:", "fonts"), ("hardware:", "hardware"), ("os:", "os")]
        .into_iter()
        .find(|(p, _)| name.starts_with(p))
        .map(|(_, g)| g)
}

/// Where a data file is in the image: `font:<file>` is
/// `/system/fonts/<file>`; `hardware:<stem>` and `os:<stem>` are
/// `/system/assets/<grant>-<stem>.fbnd`. A stem is letters, digits and
/// `-` only, so a name never reaches another file.
pub fn data_path(name: &str) -> Option<String> {
    let plain = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if let Some(file) = name.strip_prefix("font:") {
        let stem = file.strip_suffix(".ttf")?;
        return plain(stem).then(|| alloc::format!("/system/fonts/{file}"));
    }
    let (kind, stem) = name.split_once(':')?;
    (matches!(kind, "hardware" | "os") && plain(stem)).then(|| alloc::format!("/system/assets/{kind}-{stem}.fbnd"))
}

#[cfg(test)]
mod tests {
    use super::{backoff, data_grant, data_path};

    #[test]
    fn a_data_name_reaches_its_own_file_and_no_other() {
        assert_eq!(data_path("os:150").as_deref(), Some("/system/assets/os-150.fbnd"));
        assert_eq!(data_path("os:wallpaper").as_deref(), Some("/system/assets/os-wallpaper.fbnd"));
        assert_eq!(data_path("os:wallpaper-orbit").as_deref(), Some("/system/assets/os-wallpaper-orbit.fbnd"));
        assert_eq!(data_path("hardware:300").as_deref(), Some("/system/assets/hardware-300.fbnd"));
        assert_eq!(data_path("font:NotoSans-Regular.ttf").as_deref(), Some("/system/fonts/NotoSans-Regular.ttf"));
        for bad in [
            "os:../kernel",
            "os:",
            "os:150/x",
            "hardware:1.5",
            "kernel:x",
            "font:../kernel.ttf",
            "font:x.otf",
            "os:150.fbnd",
        ] {
            assert_eq!(data_path(bad), None, "{bad}");
        }
        assert_eq!(
            (data_grant("os:150"), data_grant("hardware:100"), data_grant("font:a.ttf")),
            (Some("os"), Some("hardware"), Some("fonts"))
        );
        assert_eq!(data_grant("devices:x"), None);
    }

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
