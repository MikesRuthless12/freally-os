// SPDX-License-Identifier: Apache-2.0
//! The probe, the program (TASK-069, dev only): gathers what Freally OS
//! found and draws it straight to the framebuffer, a page every 15 seconds,
//! gathering again each time round. Only `cargo xtask build --probe` carries
//! it: that image has no stage or desktop, so the screen is the probe's.
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use app_probe::{Row, Section, keyboard_verdict, ms, pages, wrap};
use fcore_abi::{perm, topic, well_known};
use fimage::blit::Surface;
use fstd::handle::Handle;
use fstd::ipc::Client;
use fstd::machine;
use fstd::time::{Duration, Instant};
use fui::draw;
use fui::layout::Rect;
use fui::text::{FontData, Fonts, words};
use fui::theme::{Mode, TextStyle, Theme, Tok};
use fwire::proto::conductor as c;
use fwire::proto::devmgr as d;
use fwire::proto::display as disp;
use fwire::proto::logger as lg;

fstd::program!(run);

/// How long each page stays up.
const PAGE_SECS: u64 = 15;

fn deadline() -> u64 {
    Instant::after(Duration::from_secs(2))
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    match Screen::open() {
        Ok(mut s) => s.show(),
        Err(e) => {
            fstd::error!("probe: {e}");
            1
        }
    }
}

/// The framebuffer, the probe's to draw on.
struct Screen {
    fb: *mut u32,
    _vmo: Handle,
    w: u32,
    h: u32,
    stride: u32,
    /// 1: RGBX (red first); 0: BGRX, which is 0x00RRGGBB as a u32.
    rgbx: bool,
    back: Vec<u32>,
    fonts: Fonts<'static>,
    theme: Theme,
}

impl Screen {
    fn open() -> Result<Screen, String> {
        let ch = fstd::service::connect_within("drv-gop-fb", Duration::from_secs(5))
            .map_err(|e| alloc::format!("drv-gop-fb: {e:?}"))?;
        let (info, mut hs) = Client::new(ch)
            .call::<_, disp::Info>(disp::INFO, &(), Vec::new(), deadline())
            .map_err(|e| alloc::format!("the framebuffer: {e:?}"))?;
        let vmo = hs.pop().ok_or("drv-gop-fb sent no framebuffer")?;
        let len = (info.stride as u64 * info.height as u64 * 4).next_multiple_of(4096);
        let at = fstd::sys::vm_map(well_known::SELF_ASPACE, vmo.raw(), 0, len, 0, perm::READ | perm::WRITE)
            .map_err(|e| String::from(e.describe()))?;
        let pct = fedid::scale_for(&machine::edid(), info.width);
        let f = |file| fstd::service::font(file).map_err(|e| alloc::format!("{file}: {}", e.describe()));
        let fonts = Fonts::new(FontData {
            regular: f("NotoSans-Regular.ttf")?,
            medium: f("NotoSans-Medium.ttf")?,
            semibold: f("NotoSans-SemiBold.ttf")?,
            mono: f("NotoSansMono-Regular.ttf")?,
            symbols: f("NotoSansSymbols2-Regular.ttf")?,
        })
        .map_err(|e| alloc::format!("the fonts: {e:?}"))?;
        Ok(Screen {
            fb: at as *mut u32,
            _vmo: vmo,
            w: info.width,
            h: info.height,
            stride: info.stride,
            rgbx: info.format == 1,
            back: alloc::vec![0; (info.width * info.height) as usize],
            fonts,
            theme: Theme::new(Mode::Dark, pct as f32 / 100.0),
        })
    }

    /// Page after page, for ever, gathering again each time round.
    fn show(&mut self) -> i64 {
        // The drivers first (a keyboard's reset takes a moment): up to 5 s for
        // every service to have started, so the first page says how it went.
        let until = Instant::after(Duration::from_secs(5));
        while Instant::now().as_ns() < until {
            let settled = fstd::service::ask::<_, c::Services>(c::LIST_SERVICES, &()).is_ok_and(|s| {
                // Not the probe itself: it never registers, so it stays "Starting".
                s.services
                    .iter()
                    .filter(|v| v.name != "probe")
                    .all(|v| !matches!(v.state, c::ServiceState::Starting | c::ServiceState::Waiting))
            });
            if settled {
                break;
            }
            fstd::time::sleep(Duration::from_millis(100));
        }
        loop {
            let sections = gather();
            let t = self.theme;
            let (_, line, _) = t.text(TextStyle::Small);
            let top = t.px(72) as i32;
            let rows = ((self.h as i32 - top - t.px(24) as i32) / line as i32).max(2) as usize;
            let col_w = t.px(560);
            let cols = ((self.w.saturating_sub(t.px(48))) / col_w).max(1) as usize;
            let all = pages(&sections, rows, cols);
            for (i, page) in all.iter().enumerate() {
                self.draw(page, i + 1, all.len(), col_w, line);
                fstd::info!("probe: page {} of {} shown", i + 1, all.len());
                fstd::time::sleep(Duration::from_secs(PAGE_SECS));
            }
        }
    }

    fn draw(&mut self, page: &[Vec<Row>], n: usize, of: usize, col_w: u32, line: u32) {
        let t = self.theme;
        let px = |v: u32| t.px(v) as i32;
        {
            let mut s = Surface { px: &mut self.back, width: self.w, height: self.h, stride: self.w };
            draw::fill(&mut s, Rect::new(0, 0, self.w, self.h), t.colour(Tok::Bg).xrgb());
            let head = alloc::format!(
                "Freally OS {} probe · page {n} of {of} · photograph each page; the next comes in {PAGE_SECS} s",
                fcore_abi::VERSION
            );
            words(&mut s, &mut self.fonts, t, TextStyle::Subtitle, (px(24), px(20)), &head, Tok::Text);
            for (k, col) in page.iter().enumerate() {
                let x = px(24) + (k as u32 * col_w) as i32;
                for (r, row) in col.iter().enumerate() {
                    let y = px(72) + (r as u32 * line) as i32;
                    let (text, tok) = match row {
                        Row::Heading(h) => (h.clone(), Tok::PrimaryInk),
                        Row::Line(l) => (self.fonts.fit(t, TextStyle::Small, l, col_w - t.px(24)), Tok::Text),
                    };
                    words(&mut s, &mut self.fonts, t, TextStyle::Small, (x, y), &text, tok);
                }
            }
        }
        // To the screen, row by row (its stride may be wider than its width).
        for y in 0..self.h as usize {
            for x in 0..self.w as usize {
                let p = self.back[y * self.w as usize + x];
                let v = if self.rgbx { ((p >> 16) & 0xFF) | (p & 0xFF00) | ((p & 0xFF) << 16) } else { p };
                // SAFETY: (x, y) is inside the mode; rows are `stride` pixels apart.
                unsafe { core::ptr::write_volatile(self.fb.add(y * self.stride as usize + x), v) };
            }
        }
    }
}

/// The CPU's brand string (CPUID 0x80000002–4) and its family, model and stepping.
fn cpu_name() -> (String, String) {
    use core::arch::x86_64::__cpuid;
    // CPUID is on every x86-64 CPU, in user mode too.
    let max = __cpuid(0x8000_0000).eax;
    let mut brand = Vec::new();
    if max >= 0x8000_0004 {
        for leaf in 0x8000_0002..=0x8000_0004u32 {
            let r = __cpuid(leaf);
            for w in [r.eax, r.ebx, r.ecx, r.edx] {
                brand.extend_from_slice(&w.to_le_bytes());
            }
        }
    }
    let brand = String::from_utf8_lossy(&brand).trim_matches(|c: char| c == '\0' || c == ' ').to_string();
    let sig = __cpuid(1).eax;
    let base_family = (sig >> 8) & 0xF;
    let family = if base_family == 0xF { base_family + ((sig >> 20) & 0xFF) } else { base_family };
    let model = if base_family == 6 || base_family == 0xF {
        ((sig >> 4) & 0xF) | ((sig >> 12) & 0xF0)
    } else {
        (sig >> 4) & 0xF
    };
    (brand, alloc::format!("family {family}, model {model:#x}, stepping {}", sig & 0xF))
}

fn read_u64s(t: u64) -> Vec<u64> {
    let mut buf = [0u8; 64];
    let n = fstd::sys::system_info(t, &mut buf).unwrap_or(0).min(buf.len());
    buf[..n].chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().unwrap_or([0; 8]))).collect()
}

/// Everything the probe shows, gathered now.
fn gather() -> Vec<Section> {
    let mut out = Vec::new();

    // The CPU.
    let mut s = Section::new("CPU");
    let (brand, sig) = cpu_name();
    s.line(if brand.is_empty() { String::from("(the CPU gives no name)") } else { brand });
    s.line(sig);
    if let Some(cpus) = machine::cpus() {
        s.line(alloc::format!(
            "{} · {} · TSC-deadline timer {}",
            cpus.layout(),
            if cpus.x2apic { "x2APIC" } else { "xAPIC" },
            if cpus.tsc_deadline { "yes" } else { "no" }
        ));
        for kind in [fcore_abi::core_type::PERFORMANCE, fcore_abi::core_type::EFFICIENCY, fcore_abi::core_type::UNKNOWN]
        {
            let ids: Vec<String> = cpus
                .cpus
                .iter()
                .filter(|c| c.core_type == kind)
                .map(|c| alloc::format!("{}{}", c.apic_id, if c.online { "" } else { "(off)" }))
                .collect();
            if ids.is_empty() {
                continue;
            }
            let label = match kind {
                fcore_abi::core_type::UNKNOWN => "APIC IDs",
                k => machine::core_type_name(k),
            };
            for (i, l) in wrap(&ids, 12).into_iter().enumerate() {
                s.line(if i == 0 { alloc::format!("{label}: {l}") } else { alloc::format!("    {l}") });
            }
        }
    }
    if let [_, hz, ..] = read_u64s(topic::CPU)[..] {
        s.line(alloc::format!("TSC {} MHz", hz / 1_000_000));
    }
    if let [free, total, ..] = read_u64s(topic::MEMORY)[..] {
        s.line(alloc::format!("Memory: {} MiB given at boot, {} MiB free", total >> 20, free >> 20));
    }
    out.push(s);

    // The display.
    let mut s = Section::new("Display");
    let (modes, current) = machine::gop_modes();
    if let Some((w, h)) = current {
        s.line(alloc::format!("In use: {w}×{h}"));
    }
    let names: Vec<String> = modes.iter().map(|(w, h)| alloc::format!("{w}×{h}")).collect();
    s.line(alloc::format!("The firmware offers {} modes:", names.len()));
    s.lines.extend(wrap(&names, 5).into_iter().map(|l| alloc::format!("    {l}")));
    let edid = machine::edid();
    match fedid::parse(&edid) {
        Ok(e) => {
            s.line(alloc::format!(
                "EDID {}.{} from {} (product {:#06x}){}",
                e.version.0,
                e.version.1,
                core::str::from_utf8(&e.manufacturer).unwrap_or("?"),
                e.product,
                e.name().map_or(String::new(), |n| alloc::format!(", \"{n}\""))
            ));
            if let Some(m) = e.preferred {
                s.line(alloc::format!(
                    "Its own mode: {}×{} at {}.{:02} Hz",
                    m.w,
                    m.h,
                    m.refresh_mhz / 1000,
                    (m.refresh_mhz % 1000) / 10
                ));
            }
            match e.size_mm {
                Some((w, h)) => s.line(alloc::format!("Size: {w} × {h} mm")),
                None => s.line("Size: not given"),
            }
        }
        Err(_) if edid.is_empty() => s.line("No EDID from the firmware"),
        Err(why) => s.line(alloc::format!("The EDID ({} bytes) was refused: {why:?}", edid.len())),
    }
    s.line(alloc::format!("UI scale for it: {} %", fedid::scale_for(&edid, current.map_or(0, |c| c.0))));
    out.push(s);

    // devmgr: the firmware's times, the keyboard, the tables, PCI.
    let services = fstd::service::ask::<_, c::Services>(c::LIST_SERVICES, &()).map(|s| s.services).unwrap_or_default();
    let state_of = |n: &str| services.iter().find(|v| v.name == n).map(|v| alloc::format!("{:?}", v.state));
    let mut dm = fstd::service::connect("devmgr").ok().map(Client::new);
    let summary = dm.as_mut().and_then(|cl| cl.call::<_, d::Summary>(d::SUMMARY, &(), Vec::new(), deadline()).ok());
    let times =
        dm.as_mut().and_then(|cl| cl.call::<_, d::FirmwareTimes>(d::FIRMWARE_TIMES, &(), Vec::new(), deadline()).ok());
    let tables = dm.as_mut().and_then(|cl| cl.call::<_, d::Tables>(d::TABLES, &(), Vec::new(), deadline()).ok());
    let pci = dm.as_mut().and_then(|cl| cl.call::<_, d::Devices>(d::LIST, &(), Vec::new(), deadline()).ok());

    let mut s = Section::new("Firmware");
    match times.map(|(t, _)| t) {
        Some(t) if t.os_loader_start > 0 => {
            s.line(alloc::format!("Its own time (FPDT): reset ended at {}", ms(t.reset_end)));
            s.line(alloc::format!("Freally OS's loader started at {}", ms(t.os_loader_start)));
        }
        _ => s.line("This firmware doesn't report its own times (no FPDT)"),
    }
    out.push(s);

    let mut s = Section::new("Keyboard");
    let summary = summary.map(|(x, _)| x);
    let has_8042 = summary.as_ref().is_none_or(|x| x.has_8042);
    s.line(keyboard_verdict(has_8042, state_of("drv-i8042").as_deref()));
    if let Some(x) = &summary {
        s.line(alloc::format!(
            "FADT 8042 flag: {} · MADT: {} CPUs · HPET: {}",
            if x.has_8042 { "present" } else { "absent" },
            x.madt_cpus,
            if x.hpet { "yes" } else { "no" }
        ));
    }
    out.push(s);

    let mut s = Section::new("Services");
    for v in &services {
        s.line(alloc::format!(
            "{}: {:?}{}",
            v.name,
            v.state,
            if v.restarts > 0 { alloc::format!(", {} restarts", v.restarts) } else { String::new() }
        ));
    }
    out.push(s);

    let mut s = Section::new("Problems logged");
    if let Ok(ch) = fstd::service::connect("logger") {
        let req = lg::Read { cursor: 0, limit: 500, filter: lg::Filter { svc: None, min_level: Some(lg::WARN) } };
        if let Ok((r, _)) = Client::new(ch).call::<_, lg::Records>(lg::READ, &req, Vec::new(), deadline()) {
            let n = r.records.len();
            for x in r.records.iter().skip(n.saturating_sub(16)) {
                s.line(alloc::format!("{} {}", x.svc, x.msg));
            }
        }
    }
    if s.lines.is_empty() {
        s.line("None");
    }
    out.push(s);

    let mut s = Section::new("ACPI tables");
    let sigs: Vec<String> = tables.map(|(t, _)| t.tables.into_iter().map(|(n, _)| n).collect()).unwrap_or_default();
    s.lines.extend(wrap(&sigs, 10));
    out.push(s);

    let mut s = Section::new("PCI");
    let bound = summary.map(|x| x.bound).unwrap_or_default();
    let pci = pci.map(|(l, _)| l.pci).unwrap_or_default();
    let cpus = machine::cpus().unwrap_or_default();
    // One line on the log for `cargo xtask probe` (and COM1 on a dev machine).
    fstd::info!(
        "probe: gathered {} CPUs ({}), {} PCI functions, {} ACPI tables; {}",
        cpus.cpus.len(),
        cpus.layout(),
        pci.len(),
        sigs.len(),
        keyboard_verdict(has_8042, state_of("drv-i8042").as_deref())
    );
    for p in pci {
        let at = alloc::format!("{:02x}:{:02x}.{}", p.bus, p.device, p.function);
        let driver = bound.iter().find(|b| b.device == at).map_or(String::new(), |b| alloc::format!(" → {}", b.driver));
        s.line(alloc::format!(
            "{at} {:04x}:{:04x} {}{driver}",
            p.vendor,
            p.id,
            machine::pci_class(p.class, p.subclass, p.prog_if)
        ));
    }
    out.push(s);
    out
}
