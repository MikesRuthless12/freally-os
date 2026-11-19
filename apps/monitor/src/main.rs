// SPDX-License-Identifier: Apache-2.0
//! Monitor, the app (TASK-062, FR-034, PRD § 8 *Monitor*): the whole system
//! on one screen, in a 960 × 640 window with tabs along the top.
//!
//! - **Processes** (updated every second): name, what it does, state,
//!   memory, CPU %, restarts; click a header to sort, again to turn it round.
//! - **Boot:** the total in the display size, a meter per stage with its
//!   milliseconds, the firmware's own times apart.
//! - **Faults:** newest first; click one to see its details.
//! - **Log:** live; the chips choose a service and a level. "No log lines
//!   match." when nothing does.
//! - **Devices** (TASK-075, TASK-070): every CPU with its core type, then
//!   what devmgr found (the screen, the PS/2 controller, the serial ports,
//!   every PCI function) and the driver that took each; the wheel, the arrow
//!   keys and Page Up/Down scroll it.
//! - Super+M opens it (the desktop starts it). If conductor doesn't answer
//!   within 500 ms: "Monitor couldn't reach the system service. It'll keep
//!   trying."
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use app_monitor::model::{self, Boot, Column, DeviceRow, FaultRow, Line, Log, Proc, Processes};
use fcore_abi::{FErr, perm, signal, well_known};
use fimage::blit::Surface;
use fstd::handle::Handle;
use fstd::ipc::{Channel, Client};
use fstd::time::{Duration, Instant};
use fui::draw;
use fui::layout::Rect;
use fui::text::{FontData, Fonts, words};
use fui::theme::{Mode, TextStyle, Theme, Tok};
use fwire::proto::ax;
use fwire::proto::conductor as c;
use fwire::proto::devmgr as d;
use fwire::proto::logger as lg;
use fwire::proto::monitor as mon;
use fwire::proto::stage as st;

fstd::program!(run);

const W: u32 = 960;
const H: u32 = 640;
const TABS: [&str; 5] = ["Processes", "Boot", "Faults", "Log", "Devices"];
const TAB_H: u32 = 44;
const ROW_H: u32 = 32;
const PAD: u32 = 16;
/// The boot's stages, by id (the protocol's own list).
const STAGES: [&str; 15] = c::STAGES;
/// The Processes table's columns: header, x at 100 %, sort column.
const COLS: [(&str, u32, Option<Column>); 6] = [
    ("Service", 16, Some(Column::Name)),
    ("What it does", 176, None),
    ("State", 520, Some(Column::State)),
    ("Memory", 620, Some(Column::Memory)),
    ("CPU", 730, Some(Column::Cpu)),
    ("Restarts", 830, Some(Column::Restarts)),
];

/// What a control does.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Act {
    Tab(usize),
    Sort(Column),
    Fault(usize),
    ServiceChip,
    LevelChip,
}

/// One control on screen: what it does, where, and what it says it is.
#[derive(Clone, Debug)]
struct Ctl {
    act: Act,
    at: (i32, i32, u32, u32),
    role: u8,
    name: String,
    testid: String,
    state: u32,
    value: String,
}

fn deadline() -> u64 {
    Instant::after(Duration::from_secs(2))
}

struct Monitor {
    stage: Client,
    window: u32,
    events: Channel,
    px: *mut u32,
    w: u32,
    h: u32,
    _vmo: Handle,
    theme: Theme,
    fonts: Fonts<'static>,
    tab: usize,
    procs: Processes,
    boot: Option<Boot>,
    faults: Vec<FaultRow>,
    open_fault: Option<usize>,
    log: Log,
    log_cursor: u64,
    logger: Option<Client>,
    devmgr: Option<Client>,
    /// Devices: the CPUs' line, the rows, and the first row shown.
    cpus_line: String,
    /// Boot: the clock, and where its frequency came from (TASK-073).
    clock: String,
    devices: Vec<DeviceRow>,
    devices_top: usize,
    /// Conductor didn't answer three refreshes running: the banner (one
    /// missed refresh keeps what was shown).
    unreachable: bool,
    missed: u32,
    last_sample: u64,
    ax: fstd::ax::Publisher,
    /// The control the keyboard is on (an index into `controls()`); none
    /// until a key moves it, and a click puts it away.
    focus: Option<usize>,
}

fn fonts() -> Result<Fonts<'static>, String> {
    let f = |file| fstd::service::font(file).map_err(|e| alloc::format!("{file}: {}", e.describe()));
    Fonts::new(FontData {
        regular: f("NotoSans-Regular.ttf")?,
        medium: f("NotoSans-Medium.ttf")?,
        semibold: f("NotoSans-SemiBold.ttf")?,
        mono: f("NotoSansMono-Regular.ttf")?,
        symbols: f("NotoSansSymbols2-Regular.ttf")?,
    })
    .map_err(|e| alloc::format!("the fonts: {e:?}"))
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    match Monitor::open() {
        Ok(mut m) => m.serve(),
        Err(e) => {
            fstd::error!("monitor: {e}");
            1
        }
    }
}

impl Monitor {
    fn open() -> Result<Monitor, String> {
        let ch = fstd::service::connect("stage").map_err(|e| alloc::format!("stage: {e:?}"))?;
        let mut stage = Client::new(ch);
        let (scr, _) = stage
            .call::<_, st::ScreenInfo>(st::SCREEN, &(), Vec::new(), deadline())
            .map_err(|e| alloc::format!("the screen: {e:?}"))?;
        let theme = Theme::new(
            if scr.theme == st::Theme::Light { Mode::Light } else { Mode::Dark },
            scr.scale_pct as f32 / 100.0,
        );
        let (w, h) = (theme.px(W), theme.px(H));
        let req = st::CreateWindow {
            title: String::from("Monitor"),
            w,
            h,
            min_w: 0,
            min_h: 0,
            kind: st::Kind::Normal,
            x: 0,
            y: 0,
        };
        let (made, mut hs) = stage
            .call::<_, st::Created>(st::CREATE_WINDOW, &req, Vec::new(), deadline())
            .map_err(|e| alloc::format!("create_window: {e:?}"))?;
        if hs.len() != 2 {
            return Err(String::from("stage sent no surface"));
        }
        let events = Channel(hs.remove(1));
        let vmo = hs.remove(0);
        let len = (w as u64 * h as u64 * 4).next_multiple_of(4096);
        let at = fstd::sys::vm_map(well_known::SELF_ASPACE, vmo.raw(), 0, len, 0, perm::READ | perm::WRITE)
            .map_err(|e| String::from(e.describe()))?;
        let mut m = Monitor {
            stage,
            window: made.window_id,
            events,
            px: at as *mut u32,
            w,
            h,
            _vmo: vmo,
            theme,
            fonts: fonts()?,
            tab: 0,
            procs: Processes::default(),
            boot: None,
            faults: Vec::new(),
            open_fault: None,
            log: Log::default(),
            log_cursor: 0,
            logger: None,
            devmgr: None,
            cpus_line: String::new(),
            clock: String::new(),
            devices: Vec::new(),
            devices_top: 0,
            unreachable: false,
            missed: 0,
            last_sample: 0,
            ax: Default::default(),
            focus: None,
        };
        m.refresh();
        m.draw();
        fstd::info!("monitor: open");
        Ok(m)
    }

    /// Everything Monitor shows, fetched again.
    fn refresh(&mut self) {
        let now = Instant::now().as_ns();
        match fstd::service::ask::<_, c::Services>(c::LIST_SERVICES, &()) {
            Ok(s) => {
                self.unreachable = false;
                self.missed = 0;
                let procs = s
                    .services
                    .into_iter()
                    .map(|v| Proc {
                        name: v.name,
                        description: v.description,
                        state: alloc::format!("{:?}", v.state),
                        pid: v.pid,
                        restarts: v.restarts,
                        mem_bytes: v.mem_bytes,
                        cpu_ns: v.cpu_ns,
                    })
                    .collect();
                self.procs.sample(procs, now);
            }
            Err(_) => {
                self.missed += 1;
                self.unreachable = self.missed >= 3;
            }
        }
        let vendor = self.read_devices();
        if let Ok(t) = fstd::service::ask::<_, c::BootTimeline>(c::BOOT_TIMELINE, &()) {
            let stages: Vec<(String, f64)> = t
                .stages
                .into_iter()
                .map(|(id, ns)| (STAGES.get(id as usize).copied().unwrap_or("?").to_string(), ns as f64 / 1e6))
                .collect();
            self.boot = Some(Boot::of(&stages).with_vendor(&vendor));
        }
        if let Ok(f) = fstd::service::ask::<_, c::Faults>(c::FAULTS, &c::Page { cursor: 0, limit: 32 }) {
            let mut rows: Vec<FaultRow> = f
                .reports
                .into_iter()
                .map(|r| FaultRow {
                    // Filed at boot from the last-panic page: no time, no pid.
                    last_boot: r.service == "freally-core" && r.ts_ns == 0 && r.pid == 0,
                    seq: r.seq,
                    service: r.service,
                    kind: alloc::format!("{:?}", r.kind),
                    message: r.message,
                    rip: r.rip,
                    addr: r.addr,
                    frames: r.frames,
                })
                .collect();
            rows.sort_by_key(|r| core::cmp::Reverse(r.seq));
            self.faults = rows;
        }
        self.read_log();
        self.last_sample = now;
    }

    /// Devices from the kernel (every CPU) and devmgr (what it found, and
    /// which driver took each); returns the firmware's vendor for Boot.
    fn read_devices(&mut self) -> String {
        let mut rows = Vec::new();
        if let Some(cpus) = fstd::machine::cpus() {
            let mut b = [0u8; 16];
            let hz = fstd::sys::system_info(fcore_abi::topic::CPU, &mut b)
                .map_or(0, |_| u64::from_le_bytes([b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]]));
            self.clock = fstd::machine::clock_line(hz, cpus.tsc_from);
            self.cpus_line = alloc::format!(
                "{} · {} · TSC-deadline timer {}",
                cpus.layout(),
                if cpus.x2apic { "x2APIC" } else { "xAPIC" },
                if cpus.tsc_deadline { "yes" } else { "no" }
            );
            for (i, c) in cpus.cpus.iter().enumerate() {
                let kind = match fstd::machine::core_type_name(c.core_type) {
                    "—" => String::from("Processor core"),
                    k => alloc::format!("{}{} core", k[..1].to_ascii_uppercase(), &k[1..]),
                };
                rows.push(DeviceRow {
                    at: alloc::format!("CPU {i}"),
                    what: alloc::format!("{kind} (APIC {}){}", c.apic_id, if c.online { "" } else { ", off" }),
                    driver: String::from("Freally Core"),
                });
            }
        }
        if self.devmgr.is_none() {
            self.devmgr = fstd::service::connect("devmgr").ok().map(Client::new);
        }
        let mut vendor = String::new();
        if let Some(dm) = self.devmgr.as_mut() {
            let summary = dm.call::<_, d::Summary>(d::SUMMARY, &(), Vec::new(), deadline()).ok().map(|(s, _)| s);
            let list = dm.call::<_, d::Devices>(d::LIST, &(), Vec::new(), deadline()).ok().map(|(l, _)| l);
            if let Ok((t, _)) = dm.call::<_, d::FirmwareTimes>(d::FIRMWARE_TIMES, &(), Vec::new(), deadline()) {
                vendor = t.vendor;
            }
            if summary.is_none() || list.is_none() {
                // devmgr restarted: connect again next time.
                self.devmgr = None;
            }
            let pci: Vec<(String, String)> = list
                .map(|l| l.pci)
                .unwrap_or_default()
                .iter()
                .map(|p| {
                    (
                        alloc::format!("{:02x}:{:02x}.{}", p.bus, p.device, p.function),
                        alloc::format!(
                            "{} ({:04x}:{:04x})",
                            fstd::machine::pci_class(p.class, p.subclass, p.prog_if),
                            p.vendor,
                            p.id
                        ),
                    )
                })
                .collect();
            let bound: Vec<(String, String)> =
                summary.map(|s| s.bound).unwrap_or_default().into_iter().map(|b| (b.device, b.driver)).collect();
            rows.extend(model::device_rows(&pci, &bound));
        }
        self.devices = rows;
        self.devices_top = self.devices_top.min(self.devices.len().saturating_sub(1));
        vendor
    }

    /// How many device rows the window shows at once.
    fn device_rows_shown(&self) -> usize {
        let t = self.theme;
        let top = t.px(TAB_H) + t.px(12) + t.px(28) + t.px(28);
        (self.h.saturating_sub(top) / t.px(ROW_H)).max(1) as usize
    }

    /// Scrolls Devices by `by` rows (negative: up), within the list.
    fn scroll_devices(&mut self, by: i32) {
        let last = self.devices.len().saturating_sub(self.device_rows_shown());
        self.devices_top = (self.devices_top as i32 + by).clamp(0, last as i32) as usize;
    }

    fn read_log(&mut self) {
        if self.logger.is_none() {
            self.logger = fstd::service::connect("logger").ok().map(Client::new);
        }
        let Some(l) = self.logger.as_mut() else { return };
        let req = lg::Read { cursor: self.log_cursor, limit: 500, filter: lg::Filter { svc: None, min_level: None } };
        if let Ok((r, _)) = l.call::<_, lg::Records>(lg::READ, &req, Vec::new(), deadline()) {
            for x in r.records {
                self.log.push(Line { ms: x.ts_ns as f64 / 1e6, service: x.svc, level: x.level, msg: x.msg });
            }
            self.log_cursor = r.next_cursor;
        }
    }

    /// The window's pixels, from the mapping (not through `self`, so the
    /// fonts can be borrowed while drawing).
    fn surface<'a>(px: *mut u32, w: u32, h: u32) -> Surface<'a> {
        // SAFETY: the surface VMO maps w × h u32s, ours to write, for the window's life.
        let px = unsafe { core::slice::from_raw_parts_mut(px, (w * h) as usize) };
        Surface { px, width: w, height: h, stride: w }
    }

    fn draw(&mut self) {
        let t = self.theme;
        let (w, h) = (self.w, self.h);
        let tab = self.tab;
        let rows = self.procs.rows.clone();
        let (sort, desc) = (self.procs.sort, self.procs.descending);
        let boot = self.boot.clone();
        let faults = self.faults.clone();
        let open_fault = self.open_fault;
        let shown: Vec<Line> = self.log.shown().into_iter().cloned().collect();
        let (svc, lvl) = (self.log.service.clone(), self.log.min_level);
        let cpus_line = self.cpus_line.clone();
        let clock = self.clock.clone();
        let devices: Vec<DeviceRow> =
            self.devices.iter().skip(self.devices_top).take(self.device_rows_shown()).cloned().collect();
        let (dev_top, dev_total) = (self.devices_top, self.devices.len());
        let unreachable = self.unreachable;
        let ring = self.focus.and_then(|i| self.controls().get(i).map(|c| c.at));
        let fonts = &mut self.fonts;
        {
            let mut s = Self::surface(self.px, w, h);
            let px = |v: u32| t.px(v) as i32;
            draw::fill(&mut s, Rect::new(0, 0, w, h), t.colour(Tok::Surface).xrgb());
            // The tabs.
            let tab_w = t.px(140);
            for (i, name) in TABS.iter().enumerate() {
                let x = px(PAD) + i as i32 * tab_w as i32;
                let on = i == tab;
                words(
                    &mut s,
                    fonts,
                    t,
                    TextStyle::Subtitle,
                    (x + px(12), px(10)),
                    name,
                    if on { Tok::Text } else { Tok::Text2 },
                );
                if on {
                    draw::fill(
                        &mut s,
                        Rect::new(x, px(TAB_H) - px(3), tab_w - t.px(8), t.px(3)),
                        t.colour(Tok::PrimaryInk).xrgb(),
                    );
                }
            }
            draw::fill(&mut s, Rect::new(0, px(TAB_H), w, 1), t.colour(Tok::Border).xrgb());
            let top = px(TAB_H) + px(12);
            if unreachable {
                words(
                    &mut s,
                    fonts,
                    t,
                    TextStyle::Body,
                    (px(PAD), top),
                    "Monitor couldn't reach the system service. It'll keep trying.",
                    Tok::Error,
                );
            } else {
                match tab {
                    0 => {
                        for (name, x, col) in COLS {
                            let arrow = match col {
                                Some(c) if c == sort => {
                                    if desc {
                                        " ↓"
                                    } else {
                                        " ↑"
                                    }
                                }
                                _ => "",
                            };
                            words(
                                &mut s,
                                fonts,
                                t,
                                TextStyle::Small,
                                (px(x), top),
                                &alloc::format!("{name}{arrow}"),
                                Tok::Text2,
                            );
                        }
                        for (i, r) in rows.iter().enumerate() {
                            let y = top + px(28) + i as i32 * px(ROW_H);
                            if y + px(ROW_H) > h as i32 {
                                break;
                            }
                            if i % 2 == 1 {
                                draw::fill(
                                    &mut s,
                                    Rect::new(px(8), y - px(6), w - t.px(16), t.px(ROW_H)),
                                    t.colour(Tok::Surface2).xrgb(),
                                );
                            }
                            let p = &r.proc;
                            words(&mut s, fonts, t, TextStyle::Body, (px(16), y), &p.name, Tok::Text);
                            let desc_text = fonts.fit(t, TextStyle::Small, &p.description, t.px(330));
                            words(&mut s, fonts, t, TextStyle::Small, (px(176), y + px(1)), &desc_text, Tok::Text2);
                            words(&mut s, fonts, t, TextStyle::Small, (px(520), y + px(1)), &p.state, Tok::Text);
                            words(
                                &mut s,
                                fonts,
                                t,
                                TextStyle::Small,
                                (px(620), y + px(1)),
                                &model::bytes(p.mem_bytes),
                                Tok::Text,
                            );
                            words(
                                &mut s,
                                fonts,
                                t,
                                TextStyle::Small,
                                (px(730), y + px(1)),
                                &model::cpu(r.cpu_pct),
                                Tok::Text,
                            );
                            words(
                                &mut s,
                                fonts,
                                t,
                                TextStyle::Small,
                                (px(830), y + px(1)),
                                &p.restarts.to_string(),
                                Tok::Text,
                            );
                        }
                    }
                    1 => {
                        if let Some(b) = &boot {
                            words(
                                &mut s,
                                fonts,
                                t,
                                TextStyle::Display,
                                (px(PAD), top),
                                &alloc::format!("{:.0} ms", b.total_ms),
                                Tok::Text,
                            );
                            words(
                                &mut s,
                                fonts,
                                t,
                                TextStyle::Small,
                                (px(PAD), top + px(40)),
                                &b.summary(),
                                Tok::Text2,
                            );
                            words(&mut s, fonts, t, TextStyle::Small, (px(PAD), top + px(60)), &clock, Tok::Text2);
                            let bar_x = px(80);
                            let bar_w = (w as i32 - bar_x - px(140)).max(1);
                            for (i, (name, ms, span)) in b.stages.iter().enumerate() {
                                let y = top + px(92) + i as i32 * px(26);
                                words(&mut s, fonts, t, TextStyle::Small, (px(PAD), y), name, Tok::Text2);
                                let r = Rect::new(bar_x, y + px(4), bar_w as u32, t.px(10));
                                draw::round_rect(&mut s, r, 3.0, t.colour(Tok::MeterOff));
                                let lit = ((bar_w as f32) * b.share(i)).max(2.0) as u32;
                                draw::round_rect(
                                    &mut s,
                                    Rect::new(bar_x, y + px(4), lit, t.px(10)),
                                    3.0,
                                    t.colour(Tok::PrimaryInk),
                                );
                                words(
                                    &mut s,
                                    fonts,
                                    t,
                                    TextStyle::Small,
                                    (bar_x + bar_w + px(12), y),
                                    &alloc::format!("{ms:.1} ms (+{span:.1})"),
                                    Tok::Text,
                                );
                            }
                        }
                    }
                    2 => {
                        if faults.is_empty() {
                            words(
                                &mut s,
                                fonts,
                                t,
                                TextStyle::Body,
                                (px(PAD), top),
                                "Nothing has crashed since Freally OS started.",
                                Tok::Text2,
                            );
                        }
                        let mut y = top;
                        for (i, f) in faults.iter().enumerate() {
                            words(
                                &mut s,
                                fonts,
                                t,
                                TextStyle::Body,
                                (px(PAD), y),
                                &alloc::format!(
                                    "#{}  {}  {}{}",
                                    f.seq,
                                    f.service,
                                    f.kind,
                                    if f.last_boot { "  · the last boot stopped here" } else { "" }
                                ),
                                Tok::Text,
                            );
                            y += px(ROW_H);
                            if open_fault == Some(i) {
                                for line in model::fault_details(f) {
                                    words(&mut s, fonts, t, TextStyle::Mono, (px(PAD + 16), y), &line, Tok::Text2);
                                    y += px(20);
                                }
                            }
                        }
                    }
                    4 => {
                        words(&mut s, fonts, t, TextStyle::Body, (px(PAD), top), &cpus_line, Tok::Text);
                        let more = if dev_total > devices.len() {
                            alloc::format!(
                                "{}–{} of {} · the wheel or the arrow keys show the rest",
                                dev_top + 1,
                                dev_top + devices.len(),
                                dev_total
                            )
                        } else {
                            alloc::format!("{dev_total} devices")
                        };
                        words(&mut s, fonts, t, TextStyle::Small, (px(560), top + px(3)), &more, Tok::Text2);
                        let head = top + px(28);
                        for (name, x) in [("Device", 16), ("What it is", 176), ("Driver", 700)] {
                            words(&mut s, fonts, t, TextStyle::Small, (px(x), head), name, Tok::Text2);
                        }
                        for (i, r) in devices.iter().enumerate() {
                            let y = head + px(28) + i as i32 * px(ROW_H);
                            if (dev_top + i) % 2 == 1 {
                                draw::fill(
                                    &mut s,
                                    Rect::new(px(8), y - px(6), w - t.px(16), t.px(ROW_H)),
                                    t.colour(Tok::Surface2).xrgb(),
                                );
                            }
                            words(&mut s, fonts, t, TextStyle::Body, (px(16), y), &r.at, Tok::Text);
                            let what = fonts.fit(t, TextStyle::Small, &r.what, t.px(510));
                            words(&mut s, fonts, t, TextStyle::Small, (px(176), y + px(1)), &what, Tok::Text2);
                            let tok = if r.driver == "—" { Tok::Text3 } else { Tok::Text };
                            words(&mut s, fonts, t, TextStyle::Small, (px(700), y + px(1)), &r.driver, tok);
                        }
                    }
                    _ => {
                        let chip = |name: &str| alloc::format!("[ {name} ]");
                        let svc_text = chip(&alloc::format!("Service: {}", svc.as_deref().unwrap_or("all")));
                        let lvl_text = chip(&alloc::format!(
                            "Level: {}",
                            ["all", "debug", "info", "warn", "error"].get(lvl as usize).copied().unwrap_or("all")
                        ));
                        words(&mut s, fonts, t, TextStyle::Small, (px(PAD), top), &svc_text, Tok::PrimaryInk);
                        words(&mut s, fonts, t, TextStyle::Small, (px(260), top), &lvl_text, Tok::PrimaryInk);
                        if shown.is_empty() {
                            words(
                                &mut s,
                                fonts,
                                t,
                                TextStyle::Body,
                                (px(PAD), top + px(32)),
                                "No log lines match.",
                                Tok::Text2,
                            );
                        }
                        let rows_fit = ((h as i32 - top - px(32)) / px(20)).max(0) as usize;
                        let from = shown.len().saturating_sub(rows_fit);
                        for (i, l) in shown[from..].iter().enumerate() {
                            let text: String = alloc::format!("{:>9.3} {:<12} {}", l.ms / 1000.0, l.service, l.msg)
                                .chars()
                                .take(110)
                                .collect();
                            let tok = match l.level {
                                4 => Tok::Error,
                                3 => Tok::Warning,
                                _ => Tok::Text,
                            };
                            words(
                                &mut s,
                                fonts,
                                t,
                                TextStyle::Mono,
                                (px(PAD), top + px(32) + i as i32 * px(20)),
                                &text,
                                tok,
                            );
                        }
                    }
                }
            }
            // The keyboard's ring, round the control it's on.
            if let Some((x, y, w, h)) = ring {
                draw::focus_ring(&mut s, Rect::new(x, y, w, h), 6.0 * t.scale, t.colour(Tok::Focus));
            }
            for p in s.px.iter_mut() {
                *p |= 0xFF00_0000;
            }
        }
        let _ = self.stage.call::<_, ()>(
            st::PRESENT,
            &st::Present { window_id: self.window, damage: Vec::new() },
            Vec::new(),
            deadline(),
        );
        let tree = self.ax_tree();
        self.ax.publish(&mut self.stage, self.window, &tree);
    }

    /// What's on screen, for the accessibility tree (TASK-063): the tabs,
    /// then the tab shown: its sort headers and table, its stages, its
    /// faults, or its chips.
    fn ax_tree(&self) -> ax::Tree {
        let t = self.theme;
        let px = |v: u32| t.px(v) as i32;
        let node = fstd::ax::node;
        let tab_w = t.px(140);
        let mut nodes = alloc::vec![node(
            ax::ROOT,
            ax::TABLIST,
            "Monitor's views",
            0,
            (px(PAD), 0, tab_w * TABS.len() as u32, t.px(TAB_H))
        )];
        for (i, c) in self.controls().into_iter().enumerate() {
            let parent = if matches!(c.act, Act::Tab(_)) { 0 } else { ax::ROOT };
            let focused = if self.focus == Some(i) { ax::FOCUSED } else { 0 };
            let mut n = node(parent, c.role, &c.name, c.state | focused, c.at);
            n.testid = c.testid;
            n.value = c.value;
            nodes.push(n);
        }
        if self.unreachable {
            nodes.push(node(
                ax::ROOT,
                ax::TEXT,
                "Monitor couldn't reach the system service.",
                0,
                (px(PAD), px(TAB_H) + px(12), 600, 24),
            ));
            return ax::Tree { nodes };
        }
        let top = px(TAB_H) + px(12);
        match self.tab {
            0 => {
                let table = nodes.len() as u32;
                let mut tb = node(
                    ax::ROOT,
                    ax::TABLE,
                    "Processes",
                    0,
                    (px(8), top + px(24), self.w - t.px(16), t.px(ROW_H) * self.procs.rows.len() as u32),
                );
                tb.testid = String::from("processes");
                nodes.push(tb);
                for (i, r) in self.procs.rows.iter().enumerate() {
                    let at = (px(8), top + px(24) + i as i32 * px(ROW_H), self.w - t.px(16), t.px(ROW_H));
                    let mut row = node(table, ax::ROW, &r.proc.name, 0, at);
                    row.value = r.proc.state.clone();
                    nodes.push(row);
                }
            }
            1 => {
                let table = nodes.len() as u32;
                let at = (px(PAD), top, self.w - t.px(2 * PAD), t.px(ROW_H) * 15);
                nodes.push(node(ax::ROOT, ax::TABLE, "Boot stages", 0, at));
                if let Some(b) = &self.boot {
                    for (i, (name, ms, _)) in b.stages.iter().enumerate() {
                        let at = (px(PAD), top + i as i32 * px(ROW_H), self.w - t.px(2 * PAD), t.px(ROW_H));
                        let mut row = node(table, ax::ROW, name, 0, at);
                        row.value = alloc::format!("{ms:.1} ms");
                        nodes.push(row);
                    }
                }
            }
            4 => {
                nodes.push(node(ax::ROOT, ax::TEXT, &self.cpus_line, 0, (px(PAD), top, 540, 24)));
                let table = nodes.len() as u32;
                let head = top + px(56);
                let shown = self.devices.iter().skip(self.devices_top).take(self.device_rows_shown());
                let mut tb = node(
                    ax::ROOT,
                    ax::TABLE,
                    "Devices",
                    0,
                    (px(8), head, self.w - t.px(16), t.px(ROW_H) * self.device_rows_shown() as u32),
                );
                tb.testid = String::from("devices");
                tb.value = alloc::format!("{} rows from {}", self.devices.len(), self.devices_top + 1);
                nodes.push(tb);
                for (i, r) in shown.enumerate() {
                    let at = (px(8), head + i as i32 * px(ROW_H), self.w - t.px(16), t.px(ROW_H));
                    let mut row = node(table, ax::ROW, &alloc::format!("{}: {}", r.at, r.what), 0, at);
                    row.value = r.driver.clone();
                    nodes.push(row);
                }
            }
            _ => {}
        }
        ax::Tree { nodes }
    }

    /// The controls on screen now, in the keyboard's order: the tabs, then
    /// the tab shown's own (its sort headers, its faults, or its chips).
    fn controls(&self) -> Vec<Ctl> {
        let t = self.theme;
        let px = |v: u32| t.px(v) as i32;
        let ctl = |act, at, role, name: &str, testid: String, state, value: &str| Ctl {
            act,
            at,
            role,
            name: String::from(name),
            testid,
            state,
            value: String::from(value),
        };
        let tab_w = t.px(140);
        let mut out: Vec<Ctl> = TABS
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let state = if i == self.tab { ax::SELECTED } else { 0 };
                let at = (px(PAD) + i as i32 * tab_w as i32, 0, tab_w, t.px(TAB_H));
                ctl(Act::Tab(i), at, ax::TAB, name, alloc::format!("tab-{}", name.to_ascii_lowercase()), state, "")
            })
            .collect();
        if self.unreachable {
            return out;
        }
        let top = px(TAB_H) + px(12);
        match self.tab {
            0 => {
                for (k, (name, x, col)) in COLS.iter().enumerate() {
                    let Some(col) = col else { continue };
                    let next = COLS.get(k + 1).map_or(W, |c| c.1);
                    let on = *col == self.procs.sort;
                    let value = match (on, self.procs.descending) {
                        (false, _) => "",
                        (true, true) => "descending",
                        (true, false) => "ascending",
                    };
                    out.push(ctl(
                        Act::Sort(*col),
                        (px(*x), top, t.px(next - x), t.px(24)),
                        ax::BUTTON,
                        &alloc::format!("Sort by {name}"),
                        alloc::format!("sort-{}", name.to_ascii_lowercase()),
                        if on { ax::PRESSED } else { 0 },
                        value,
                    ));
                }
            }
            2 => {
                let mut ry = top;
                for (i, f) in self.faults.iter().enumerate() {
                    let open = self.open_fault == Some(i);
                    out.push(ctl(
                        Act::Fault(i),
                        (px(8), ry, self.w - t.px(16), t.px(ROW_H)),
                        ax::BUTTON,
                        &alloc::format!("{}: {}", f.service, f.message),
                        alloc::format!("fault-{}", f.seq),
                        if open { ax::EXPANDED } else { 0 },
                        "",
                    ));
                    ry += px(ROW_H) + if open { px(20) * (3 + f.frames.len() as i32) } else { 0 };
                }
            }
            3 => {
                let svc = alloc::format!("Service: {}", self.log.service.as_deref().unwrap_or("all"));
                let lvl = alloc::format!(
                    "Level: {}",
                    ["all", "debug", "info", "warn", "error"]
                        .get(self.log.min_level as usize)
                        .copied()
                        .unwrap_or("all")
                );
                let at = (px(PAD), top, t.px(240), t.px(24));
                out.push(ctl(Act::ServiceChip, at, ax::BUTTON, &svc, String::from("chip-service"), 0, ""));
                let at = (px(260), top, t.px(200), t.px(24));
                out.push(ctl(Act::LevelChip, at, ax::BUTTON, &lvl, String::from("chip-level"), 0, ""));
            }
            _ => {}
        }
        out
    }

    /// Works a control (a click on it, or Enter or Space on it).
    fn act(&mut self, a: Act) {
        match a {
            Act::Tab(i) => {
                self.tab = i;
                fstd::info!("monitor: tab {}", TABS[i]);
            }
            Act::Sort(c) => self.procs.sort_by(c),
            Act::Fault(i) => self.open_fault = if self.open_fault == Some(i) { None } else { Some(i) },
            Act::ServiceChip => {
                // All, then each service in turn.
                let svcs = self.log.services();
                self.log.service = match &self.log.service {
                    None => svcs.first().cloned(),
                    Some(cur) => svcs.iter().position(|s| s == cur).and_then(|i| svcs.get(i + 1).cloned()),
                };
            }
            Act::LevelChip => self.log.min_level = (self.log.min_level + 1) % 5,
        }
    }

    /// A click at (x, y) on the surface: the control there, if any; the
    /// keyboard's ring goes away.
    fn click(&mut self, x: i32, y: i32) {
        self.focus = None;
        let hit = self.controls().into_iter().find(|c| {
            let (cx, cy, w, h) = c.at;
            x >= cx && y >= cy && x < cx + w as i32 && y < cy + h as i32
        });
        if let Some(c) = hit {
            self.act(c.act);
        }
    }

    /// Tab and Shift+Tab move the keyboard's focus round the controls;
    /// Enter and Space work the one it's on. True if anything changed.
    fn keyboard(&mut self, usage: u16, modifiers: u8) -> bool {
        const TAB: u16 = 0x2B;
        const ENTER: u16 = 0x28;
        const SPACE: u16 = 0x2C;
        let n = self.controls().len();
        match usage {
            TAB if modifiers & 2 == 0 && n > 0 => {
                let back = modifiers & 1 != 0;
                self.focus = Some(match (self.focus, back) {
                    (None, false) => 0,
                    (None, true) => n - 1,
                    (Some(i), false) => (i + 1) % n,
                    (Some(i), true) => (i + n - 1) % n,
                });
                true
            }
            ENTER | SPACE => {
                let Some(c) = self.focus.and_then(|i| self.controls().get(i).cloned()) else { return false };
                self.act(c.act);
                // The controls may have changed (another tab): stay in range.
                let n = self.controls().len();
                self.focus = self.focus.map(|i| i.min(n.saturating_sub(1)));
                true
            }
            _ => false,
        }
    }

    fn event(&mut self, ordinal: u32, p: &[u8]) -> bool {
        match ordinal {
            st::EV_POINTER => {
                if let Ok(e) = fwire::from_payload::<st::Pointer>(p) {
                    if e.kind == st::PointerKind::Down && e.buttons & 1 != 0 {
                        self.click(e.x, e.y);
                        self.draw();
                    } else if e.kind == st::PointerKind::Wheel && self.tab == 4 && e.dy != 0 {
                        // Up the wheel (dy positive) shows earlier rows, three at a notch.
                        self.scroll_devices(-e.dy.signum() * 3);
                        self.draw();
                    }
                }
            }
            st::EV_KEY => {
                if let Ok(k) = fwire::from_payload::<st::Key>(p)
                    && k.down
                {
                    // Devices scroll by the arrows, a page, or to either end.
                    let page = self.device_rows_shown() as i32;
                    let scroll = match k.usage {
                        0x52 => Some(-1),
                        0x51 => Some(1),
                        0x4B => Some(-page),
                        0x4E => Some(page),
                        0x4A => Some(i32::MIN / 2),
                        0x4D => Some(i32::MAX / 2),
                        _ => None,
                    };
                    if self.tab == 4
                        && let Some(by) = scroll
                    {
                        self.scroll_devices(by);
                        self.draw();
                    } else if k.usage == 0x2B && k.modifiers & 2 != 0 {
                        // Ctrl+Tab: the next tab.
                        self.tab = (self.tab + 1) % TABS.len();
                        self.draw();
                    } else if self.keyboard(k.usage, k.modifiers) {
                        self.draw();
                    }
                }
            }
            st::EV_THEME => {
                if let Ok(t) = fwire::from_payload::<st::SetTheme>(p) {
                    self.theme.mode = if t.theme == st::Theme::Light { Mode::Light } else { Mode::Dark };
                    self.draw();
                }
            }
            st::EV_CLOSE_REQUESTED => return false,
            _ => {}
        }
        true
    }

    /// The e2e door (conductor only): what Monitor shows, as text.
    fn door(&mut self, ch: &Channel, name: &str) -> bool {
        loop {
            let m = match ch.try_recv() {
                Ok(m) => m,
                Err(FErr::ShouldWait) => return true,
                Err(_) => return false,
            };
            let Ok((h, _)) = fwire::parse(&m.bytes) else { continue };
            let reply = if name != "conductor" || fstd::service::flavour() != 2 {
                fwire::error_reply(
                    h.txid,
                    h.ordinal,
                    FErr::AccessDenied as i32,
                    "the test door is conductor's, in e2e images",
                )
            } else if h.ordinal == mon::STATE {
                fwire::reply(h.txid, h.ordinal, &self.state())
            } else {
                fwire::error_reply(h.txid, h.ordinal, FErr::NotSupported as i32, "not served")
            };
            if let Ok(r) = reply {
                let _ = ch.send(&r, Vec::new());
            }
        }
    }

    fn state(&self) -> mon::State {
        let lines: Vec<String> = match self.tab {
            0 => self
                .procs
                .rows
                .iter()
                .map(|r| {
                    alloc::format!(
                        "{} | {} | {} | {} | {}",
                        r.proc.name,
                        r.proc.state,
                        model::bytes(r.proc.mem_bytes),
                        model::cpu(r.cpu_pct),
                        r.proc.restarts
                    )
                })
                .collect(),
            1 => self.boot.as_ref().map_or(Vec::new(), |b| {
                let mut v: Vec<String> = b.stages.iter().map(|(n, ms, _)| alloc::format!("{n} {ms:.1}")).collect();
                v.push(b.summary());
                v.push(self.clock.clone());
                v
            }),
            2 => {
                let mut v: Vec<String> = self
                    .faults
                    .iter()
                    .map(|f| {
                        alloc::format!(
                            "#{} {} {}{}",
                            f.seq,
                            f.service,
                            f.kind,
                            if f.last_boot { " (the last boot)" } else { "" }
                        )
                    })
                    .collect();
                if let Some(f) = self.open_fault.and_then(|i| self.faults.get(i)) {
                    v.extend(model::fault_details(f));
                }
                v
            }
            3 => self.log.shown().iter().map(|l| alloc::format!("{} {}", l.service, l.msg)).collect(),
            _ => {
                let mut v = alloc::vec![self.cpus_line.clone()];
                v.extend(self.devices.iter().map(|r| alloc::format!("{} | {} | {}", r.at, r.what, r.driver)));
                v.push(alloc::format!("top {}", self.devices_top));
                v
            }
        };
        mon::State {
            tab: String::from(TABS[self.tab]),
            sort: alloc::format!("{:?}{}", self.procs.sort, if self.procs.descending { " down" } else { " up" }),
            filter: alloc::format!("{} {}", self.log.service.as_deref().unwrap_or("all"), self.log.min_level),
            lines,
            window: self.window,
        }
    }

    fn serve(&mut self) -> i64 {
        let Ok((listen, give)) = Channel::pair() else { return 1 };
        if fstd::service::register(give).is_err() {
            return 1;
        }
        let mut clients: Vec<(Channel, String)> = Vec::new();
        loop {
            let mut items = alloc::vec![
                [self.events.0.raw(), signal::READABLE | signal::PEER_CLOSED],
                [listen.0.raw(), signal::READABLE]
            ];
            items.extend(clients.iter().map(|(c, _)| [c.0.raw(), signal::READABLE | signal::PEER_CLOSED]));
            // A second between samples.
            let until = self.last_sample + 1_000_000_000;
            match fstd::sys::wait_many(&items, until) {
                Ok(0) => loop {
                    match self.events.try_recv() {
                        Ok(m) => {
                            if let Ok((h, p)) = fwire::parse(&m.bytes)
                                && !self.event(h.ordinal, p)
                            {
                                fstd::info!("monitor: closed");
                                return 0;
                            }
                        }
                        Err(FErr::ShouldWait) => break,
                        Err(_) => return 0,
                    }
                },
                Ok(1) => {
                    while let Ok(m) = listen.try_recv() {
                        if let Ok((h, p)) = fwire::parse(&m.bytes)
                            && h.ordinal == c::INCOMING
                            && let Ok(inc) = fwire::from_payload::<c::Incoming>(p)
                            && let Some(ch) = m.handles.into_iter().next()
                        {
                            clients.push((Channel(ch), inc.client));
                        }
                    }
                }
                Ok(k) => {
                    let i = k - 2;
                    let (ch, name) = &clients[i];
                    if !self.door(ch, name) {
                        clients.remove(i);
                    }
                }
                Err(_) => {}
            }
            if Instant::now().as_ns() >= self.last_sample + 1_000_000_000 {
                self.refresh();
                self.draw();
            }
        }
    }
}
