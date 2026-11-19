// SPDX-License-Identifier: Apache-2.0
//! stage, the service (TASK-057, FR-028): owns the framebuffer drv-gop-fb
//! offers, builds the screen in a back buffer, hears drv-i8042's keys and
//! mouse, and serves windows to its clients (PRD § 4, *Stage*).
//!
//! - Each window's surface is a VMO stage makes and maps read-only; the
//!   client gets it writable, draws, and `present`s the parts it changed.
//! - Every change (a present, a move, a focus, a theme) is damage; after each
//!   batch of messages stage composes the damage and copies it to the screen.
//! - S3: stamped with conductor once the first frame is on the screen.
//! - The fleet cursor (TASK-058) is drawn over every present, never into the
//!   back buffer, from the very first frame; it asks for frames only while
//!   it moves.
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::{FErr, perm, rights, signal, well_known};
use fstd::handle::Handle;
use fstd::ipc::{Channel, Client};
use fstd::time::{Duration, Instant};
use fui::text::{FontData, Fonts};
use fui::theme::{Mode, Theme};
use fwire::HandleRef;
use fwire::proto::conductor as c;
use fwire::proto::display as disp;
use fwire::proto::input as inp;
use fwire::proto::stage as st;
use svc_stage::compose::{self, Back, Look, Pixels, Shared};
use svc_stage::cursor::{self, Cursor, Inks, Kind as CursorKind};
use svc_stage::input::{Desk, Effect, Event};
use svc_stage::windows::Part;
use svc_stage::windows::{Rect, Windows};

fstd::program!(run);

/// The largest surface stage makes: 8192 × 8192.
const MAX_SIDE: u32 = 8192;
/// A request's answer: the reply body and the handles it carries, or why not.
type Reply = Result<(Vec<u8>, Vec<Handle>), (FErr, &'static str)>;

/// Damage kept as separate rectangles up to this many, then merged.
const MAX_DAMAGE: usize = 16;

struct Screen {
    px: *mut u32,
    w: u32,
    h: u32,
    stride: u32,
    format: u32,
}

struct Conn {
    id: u32,
    name: String,
    ch: Channel,
}

struct Win {
    id: u32,
    events: Channel,
    _vmo: Handle,
    map: (u64, u64),
    pixels: Shared,
    /// What the pointer looks like over its surface (`set_cursor`).
    cursor: CursorKind,
    /// Its accessibility tree, as last published, and its length.
    ax: Option<(Handle, u64)>,
}

struct Stage {
    desk: Desk,
    back: Back,
    screen: Screen,
    theme: Theme,
    fonts: Fonts<'static>,
    wins: Vec<Win>,
    conns: Vec<Conn>,
    next_conn: u32,
    damage: Vec<Rect>,
    /// Screen areas to put on the framebuffer again without composing:
    /// where the cursor was and is.
    refresh: Vec<Rect>,
    cursor: Cursor,
    /// The desktop's shortcuts channel, once it subscribes.
    shortcuts: Option<Channel>,
    stamped: bool,
    /// The OS's renders (TASK-052), when the image carries them.
    art: Option<Art>,
    /// The screensaver's timer (TASK-077C).
    idle: svc_stage::idle::Idle,
}

/// Who may read back the windows and the pointer: the test door (conductor
/// and cdpd, e2e images) and the Brain (dev images).
#[cfg(any(feature = "flavour-e2e", feature = "flavour-dev"))]
fn reads_back(name: &str) -> bool {
    (cfg!(feature = "flavour-e2e") && matches!(name, "conductor" | "cdpd"))
        || (cfg!(feature = "flavour-dev") && name == "brain")
}

fn rdtsc() -> u64 {
    // SAFETY: RDTSC is allowed in user mode (CR4.TSD is clear).
    unsafe { core::arch::x86_64::_rdtsc() }
}

fn screen() -> Result<Screen, String> {
    let ch = fstd::service::connect("drv-gop-fb").map_err(|e| alloc::format!("drv-gop-fb: {e:?}"))?;
    let (info, mut hs) = Client::new(ch)
        .call::<_, disp::Info>(disp::INFO, &(), Vec::new(), Instant::after(Duration::from_secs(2)))
        .map_err(|e| alloc::format!("the framebuffer: {e:?}"))?;
    let vmo = (!hs.is_empty()).then(|| hs.remove(0)).ok_or("no framebuffer VMO")?;
    let len = (info.stride as u64 * info.height as u64 * 4).next_multiple_of(4096);
    let at = fstd::sys::vm_map(well_known::SELF_ASPACE, vmo.raw(), 0, len, 0, perm::READ | perm::WRITE)
        .map_err(|e| alloc::format!("mapping the framebuffer: {}", e.describe()))?;
    core::mem::forget(vmo);
    Ok(Screen { px: at as *mut u32, w: info.width, h: info.height, stride: info.stride, format: info.format })
}

/// The OS's renders (TASK-052) for both themes, from the `os` grant's 1x
/// bundle; a theme's wallpaper is scaled to the screen the first time it's
/// shown.
struct Art {
    bundle: fimage::bundle::Bundle<'static>,
    dark: Option<compose::ThemeArt<'static>>,
    light: Option<compose::ThemeArt<'static>>,
}

impl Art {
    fn load() -> Option<Art> {
        let bytes = fstd::service::data("os:1x").ok()?;
        let bundle = fimage::bundle::Bundle::parse(bytes).ok()?;
        Some(Art { bundle, dark: None, light: None })
    }

    fn for_theme(&mut self, mode: Mode, w: u32, h: u32) -> Option<&compose::ThemeArt<'static>> {
        let (name, slot) = if mode == Mode::Light { ("light", &mut self.light) } else { ("dark", &mut self.dark) };
        if slot.is_none() {
            let get = |piece: &str| {
                self.bundle.get(&alloc::format!("{name}/{piece}")).and_then(|b| fimage::fimg::parse(b).ok())
            };
            let (wall, strip, strip_focused) = (get("wallpaper")?, get("title_strip")?, get("title_strip_focused")?);
            *slot = Some(compose::ThemeArt { ground: fimage::blit::cover(&wall, w, h), strip, strip_focused });
        }
        slot.as_ref()
    }
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

/// drv-i8042's events, if there's a PS/2 controller.
fn input() -> Option<Channel> {
    let ch = fstd::service::connect_within("drv-i8042", Duration::from_secs(3)).ok()?;
    let mut client = Client::new(ch);
    client.call::<_, ()>(inp::SUBSCRIBE, &(), Vec::new(), Instant::after(Duration::from_secs(2))).ok()?;
    Some(client.channel)
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    let made = screen().and_then(|s| fonts().map(|f| (s, f)));
    let (screen, fonts) = match made {
        Ok(v) => v,
        Err(e) => {
            fstd::error!("stage: {e}");
            return 1;
        }
    };
    // The scale the display's own size suits (TASK-071): 150 % on the OMEN's
    // 16-inch 2560×1600 panel, 100 % in a VM.
    let pct = fedid::scale_for(&fstd::machine::edid(), screen.w);
    fstd::info!("stage: {}x{}, starting at {pct} %", screen.w, screen.h);
    let theme = Theme::new(Mode::Dark, pct as f32 / 100.0);
    let strip = theme.px(fui::tokens_os::TITLE_STRIP_H);
    let mut s = Stage {
        desk: Desk::new(Windows::new(screen.w, screen.h, strip)),
        back: Back::new(screen.w, screen.h),
        theme,
        fonts,
        wins: Vec::new(),
        conns: Vec::new(),
        next_conn: 1,
        damage: alloc::vec![Rect::new(0, 0, screen.w, screen.h)],
        refresh: Vec::new(),
        cursor: Cursor::new(screen.w as i32 / 2, screen.h as i32 / 2, Instant::now().as_ns()),
        idle: svc_stage::idle::Idle::new(Instant::now().as_ns()),
        shortcuts: None,
        stamped: false,
        art: Art::load(),
        screen,
    };
    let Ok((listen, give)) = Channel::pair() else { return 1 };
    if fstd::service::register(give).is_err() {
        return 1;
    }
    let keys = input();
    if keys.is_none() {
        fstd::warn!("stage: no keyboard or mouse (drv-i8042 isn't serving)");
    }
    // The first frame (S3) once the input's settled: S2 always comes first.
    s.flush();
    loop {
        let mut items = alloc::vec![[listen.0.raw(), signal::READABLE]];
        if let Some(k) = &keys {
            items.push([k.0.raw(), signal::READABLE]);
        }
        let first_conn = items.len();
        items.extend(s.conns.iter().map(|c| [c.ch.0.raw(), signal::READABLE | signal::PEER_CLOSED]));
        // Frames only while the cursor moves; otherwise sleep until something happens.
        let mut until =
            if s.cursor.moving(Instant::now().as_ns()) { Instant::after(Duration::from_millis(16)) } else { u64::MAX };
        until = until.min(s.idle.deadline().unwrap_or(u64::MAX));
        match fstd::sys::wait_many(&items, until) {
            Ok(0) => {
                while let Ok(m) = listen.try_recv() {
                    s.incoming(m);
                }
            }
            Ok(k) if k < first_conn => {
                if let Some(ch) = &keys {
                    while let Ok(m) = ch.try_recv() {
                        s.input(&m.bytes);
                    }
                }
            }
            Ok(k) => s.serve(k - first_conn),
            Err(FErr::TimedOut) => {}
            Err(_) => fstd::time::sleep(Duration::from_millis(10)),
        }
        if s.idle.start_if_due(Instant::now().as_ns()) {
            s.saver(true);
        }
        s.animate();
        s.flush();
    }
}

impl Stage {
    fn incoming(&mut self, m: fstd::ipc::Message) {
        let Ok((h, p)) = fwire::parse(&m.bytes) else { return };
        let Ok(inc) = fwire::from_payload::<c::Incoming>(p) else { return };
        if h.ordinal != c::INCOMING {
            return;
        }
        if let Some(ch) = m.handles.into_iter().next() {
            self.conns.push(Conn { id: self.next_conn, name: inc.client, ch: Channel(ch) });
            self.next_conn += 1;
        }
    }

    /// Serves every message waiting on connection `i`; a connection gone
    /// takes its windows with it.
    fn serve(&mut self, i: usize) {
        loop {
            match self.conns[i].ch.try_recv() {
                Ok(m) => self.request(i, m),
                Err(FErr::ShouldWait) => return,
                Err(_) => {
                    let gone = self.conns.remove(i);
                    for id in self.desk.windows.owned_by(gone.id) {
                        self.close(id);
                    }
                    return;
                }
            }
        }
    }

    fn request(&mut self, i: usize, m: fstd::ipc::Message) {
        let Ok((h, p)) = fwire::parse(&m.bytes) else { return };
        let (conn, name) = (self.conns[i].id, self.conns[i].name.clone());
        let r: Reply = match h.ordinal {
            st::CREATE_WINDOW => fwire::from_payload(p)
                .map_err(|_| (FErr::InvalidArgs, "unreadable"))
                .and_then(|cw| self.create(conn, &name, cw)),
            st::PRESENT => fwire::from_payload::<st::Present>(p)
                .map_err(|_| (FErr::InvalidArgs, "unreadable"))
                .and_then(|pr| self.present(conn, pr).map(|_| (Vec::new(), Vec::new()))),
            st::SET_TITLE => {
                fwire::from_payload::<st::SetTitle>(p).map_err(|_| (FErr::InvalidArgs, "unreadable")).and_then(|t| {
                    self.own(conn, t.window_id)?;
                    if let Some(r) = self.desk.windows.set_title(t.window_id, &t.title) {
                        self.damage(r);
                    }
                    Ok((Vec::new(), Vec::new()))
                })
            }
            st::CLOSE_WINDOW => {
                fwire::from_payload::<st::ById>(p).map_err(|_| (FErr::InvalidArgs, "unreadable")).and_then(|b| {
                    self.own(conn, b.window_id)?;
                    self.close(b.window_id);
                    Ok((Vec::new(), Vec::new()))
                })
            }
            st::FOCUS_WINDOW => {
                fwire::from_payload::<st::ById>(p).map_err(|_| (FErr::InvalidArgs, "unreadable")).and_then(|b| {
                    // The test door may bring any window forward (e2e images:
                    // CDP's Target.activateTarget); anyone else, their own.
                    let door = cfg!(feature = "flavour-e2e") && matches!(name.as_str(), "conductor" | "cdpd");
                    if door {
                        self.desk.windows.get(b.window_id).ok_or((FErr::NotFound, "no such window"))?;
                    } else {
                        self.own(conn, b.window_id)?;
                    }
                    let mut out = Vec::new();
                    self.desk.focus(b.window_id, &mut out);
                    self.apply(out);
                    Ok((Vec::new(), Vec::new()))
                })
            }
            st::SET_CURSOR => {
                fwire::from_payload::<st::SetCursor>(p).map_err(|_| (FErr::InvalidArgs, "unreadable")).and_then(|c| {
                    self.own(conn, c.window_id)?;
                    let kind = CursorKind::from_u8(c.kind).ok_or((FErr::InvalidArgs, "no such cursor"))?;
                    if let Some(w) = self.wins.iter_mut().find(|w| w.id == c.window_id) {
                        w.cursor = kind;
                    }
                    self.point();
                    Ok((Vec::new(), Vec::new()))
                })
            }
            st::SET_SCALE => {
                fwire::from_payload::<st::SetScale>(p).map_err(|_| (FErr::InvalidArgs, "unreadable")).and_then(|sc| {
                    if !matches!(name.as_str(), "settings" | "shell" | "terminal" | "cdpd" | "conductor") {
                        return Err((FErr::AccessDenied, "only settings, the shell and cdpd set the scale"));
                    }
                    if !(100..=300).contains(&sc.pct) || sc.pct % 25 != 0 {
                        return Err((FErr::InvalidArgs, "a scale from 100 to 300 %, in steps of 25"));
                    }
                    self.set_scale(sc.pct);
                    Ok((Vec::new(), Vec::new()))
                })
            }
            st::SCREEN => Ok((
                fwire::to_payload(&st::ScreenInfo {
                    w: self.screen.w,
                    h: self.screen.h,
                    scale_pct: (self.theme.scale * 100.0 + 0.5) as u16,
                    theme: if self.theme.mode == Mode::Light { st::Theme::Light } else { st::Theme::Dark },
                }),
                Vec::new(),
            )),
            st::SUBSCRIBE_SHORTCUTS => {
                if name != "desktop" {
                    Err((FErr::AccessDenied, "the shortcuts are the desktop's"))
                } else {
                    match Channel::pair() {
                        Ok((mine, theirs)) => {
                            self.shortcuts = Some(mine);
                            Ok((fwire::to_payload(&st::Subscribed { events: HandleRef(0) }), alloc::vec![theirs.0]))
                        }
                        Err(_) => Err((FErr::NoMemory, "no channel")),
                    }
                }
            }
            st::SCREENSHOT => {
                fwire::from_payload::<st::Screenshot>(p).map_err(|_| (FErr::InvalidArgs, "unreadable")).and_then(|s| {
                    if !matches!(name.as_str(), "brain" | "cdpd") {
                        return Err((FErr::AccessDenied, "only the Brain and cdpd take screenshots"));
                    }
                    self.screenshot(s.window_id)
                })
            }
            st::INJECT_INPUT => {
                fwire::from_payload::<st::Inject>(p).map_err(|_| (FErr::InvalidArgs, "unreadable")).and_then(|inj| {
                    let allowed = matches!((name.as_str(), fstd::service::flavour()), ("brain", 1) | ("cdpd", 2));
                    if !allowed {
                        return Err((FErr::AccessDenied, "only the Brain (dev) and cdpd (e2e) inject input"));
                    }
                    if let Some(k) = inj.key {
                        self.key(k);
                    }
                    if let Some(m) = inj.mouse {
                        self.mouse(m);
                    }
                    Ok((Vec::new(), Vec::new()))
                })
            }
            st::PUBLISH_AX_TREE => {
                let tree = m.handles.into_iter().next();
                fwire::from_payload::<st::AxTree>(p).map_err(|_| (FErr::InvalidArgs, "unreadable")).and_then(|a| {
                    self.own(conn, a.window_id)?;
                    let tree = tree.ok_or((FErr::InvalidArgs, "no tree VMO"))?;
                    let mut len = [0u8; 8];
                    fstd::sys::vmo_read(tree.raw(), 0, &mut len).map_err(|_| (FErr::InvalidArgs, "unreadable tree"))?;
                    if let Some(w) = self.wins.iter_mut().find(|w| w.id == a.window_id) {
                        w.ax = Some((tree, u64::from_le_bytes(len)));
                    }
                    Ok((Vec::new(), Vec::new()))
                })
            }
            st::RAISE_APP if name == "desktop" => {
                fwire::from_payload::<st::RaiseApp>(p).map_err(|_| (FErr::InvalidArgs, "unreadable")).map(|r| {
                    // Its most recently used window, if it has one.
                    let id = self
                        .desk
                        .windows
                        .cycle_order()
                        .into_iter()
                        .find(|&id| self.desk.windows.get(id).is_some_and(|w| w.owner == r.name));
                    if let Some(id) = id {
                        let mut out = Vec::new();
                        self.desk.focus(id, &mut out);
                        self.apply(out);
                    }
                    (fwire::to_payload(&id.is_some()), Vec::new())
                })
            }
            st::SET_SAVER | st::SET_MOTION => {
                let door = cfg!(feature = "flavour-e2e") && matches!(name.as_str(), "conductor" | "cdpd");
                if !door && name != "settings" {
                    Err((FErr::AccessDenied, "only settings (and the e2e door) choose the screensaver"))
                } else if h.ordinal == st::SET_SAVER {
                    fwire::from_payload::<st::SetSaver>(p).map_err(|_| (FErr::InvalidArgs, "unreadable")).map(|v| {
                        self.idle.set(v.kind.min(3), v.idle_ms as u64 * 1_000_000, Instant::now().as_ns());
                        fstd::info!("stage: the screensaver is {} after {} ms idle", v.kind.min(3), v.idle_ms);
                        (Vec::new(), Vec::new())
                    })
                } else {
                    fwire::from_payload::<st::SetMotion>(p).map_err(|_| (FErr::InvalidArgs, "unreadable")).map(|v| {
                        self.idle.reduce_motion = v.reduce;
                        self.cursor.reduce_motion = v.reduce;
                        fstd::info!("stage: Reduce Motion {}", if v.reduce { "on" } else { "off" });
                        (Vec::new(), Vec::new())
                    })
                }
            }
            st::SET_THEME => {
                fwire::from_payload::<st::SetTheme>(p).map_err(|_| (FErr::InvalidArgs, "unreadable")).and_then(|t| {
                    let brain = cfg!(feature = "flavour-dev") && name == "brain";
                    if !brain && !matches!(name.as_str(), "settings" | "shell" | "terminal" | "cdpd" | "conductor") {
                        return Err((FErr::AccessDenied, "only settings, the shell, the Brain and cdpd set the theme"));
                    }
                    self.set_theme(t.theme);
                    Ok((Vec::new(), Vec::new()))
                })
            }
            #[cfg(any(feature = "flavour-e2e", feature = "flavour-dev"))]
            st::LIST_WINDOWS if reads_back(&name) => Ok((fwire::to_payload(&self.list()), Vec::new())),
            #[cfg(feature = "flavour-e2e")]
            st::AX_TREE if matches!(name.as_str(), "conductor" | "cdpd") => fwire::from_payload::<st::ById>(p)
                .map_err(|_| (FErr::InvalidArgs, "unreadable"))
                .map(|b| (fwire::to_payload(&self.ax_bytes(b.window_id)), Vec::new())),
            #[cfg(any(feature = "flavour-e2e", feature = "flavour-dev"))]
            st::POINTER if reads_back(&name) => Ok((
                fwire::to_payload(&st::PointerAt { x: self.desk.x, y: self.desk.y, buttons: self.desk.buttons }),
                Vec::new(),
            )),
            _ => Err((FErr::NotSupported, "stage doesn't serve that (yet)")),
        };
        let msg = match r {
            Ok((body, hs)) => fwire::message_bytes(h.txid, h.ordinal, fwire::flags::REPLY, body).map(|m| (m, hs)),
            Err((e, text)) => fwire::error_reply(h.txid, h.ordinal, e as i32, text).map(|m| (m, Vec::new())),
        };
        if let Ok((m, hs)) = msg {
            let _ = self.conns[i].ch.send(&m, hs);
        }
    }

    fn own(&self, conn: u32, id: u32) -> Result<(), (FErr, &'static str)> {
        match self.desk.windows.get(id) {
            Some(w) if w.client == conn => Ok(()),
            Some(_) => Err((FErr::AccessDenied, "not your window")),
            None => Err((FErr::NotFound, "no such window")),
        }
    }

    fn create(&mut self, conn: u32, name: &str, cw: st::CreateWindow) -> Reply {
        if cw.w == 0 || cw.h == 0 || cw.w > MAX_SIDE || cw.h > MAX_SIDE || cw.title.len() > 256 {
            return Err((FErr::InvalidArgs, "a window from 1 × 1 to 8192 × 8192, its title ≤ 256 bytes"));
        }
        let mut out = Vec::new();
        let id = self.desk.create(
            &cw.title,
            cw.kind,
            (cw.w, cw.h),
            (cw.min_w, cw.min_h),
            (cw.x, cw.y),
            (conn, name),
            &mut out,
        );
        let made = self.surface(id);
        let Some((vmo, theirs, map, pixels, events, their_events)) = made else {
            let mut ignore = Vec::new();
            self.desk.close(id, &mut ignore);
            return Err((FErr::NoMemory, "no room for the surface"));
        };
        self.wins.push(Win { id, events, _vmo: vmo, map, pixels, cursor: CursorKind::Arrow, ax: None });
        self.apply(out);
        let reply = fwire::to_payload(&st::Created { window_id: id, surface: HandleRef(0), events: HandleRef(1) });
        Ok((reply, alloc::vec![theirs, their_events]))
    }

    /// A window's surface (made, mapped, a writable duplicate for the client)
    /// and its event channel's two ends.
    #[allow(clippy::type_complexity)]
    fn surface(&self, id: u32) -> Option<(Handle, Handle, (u64, u64), Shared, Channel, Handle)> {
        let w = self.desk.windows.get(id)?;
        let (sw, sh) = (w.w, w.h);
        let len = (sw as u64 * sh as u64 * 4).next_multiple_of(4096);
        let vmo = Handle::from_raw(fstd::sys::vmo_create(len).ok()?)?;
        let at = fstd::sys::vm_map(well_known::SELF_ASPACE, vmo.raw(), 0, len, 0, perm::READ).ok()?;
        // SAFETY: just mapped, `len` ≥ sw × sh × 4 bytes, unmapped only when the window closes.
        let pixels = unsafe { Shared::new(at as *const u32, sw, sh) };
        let theirs = vmo.duplicate(rights::READ | rights::WRITE | rights::MAP | rights::TRANSFER).ok()?;
        let (mine, their_events) = Channel::pair().ok()?;
        Some((vmo, theirs, (at, len), pixels, mine, their_events.0))
    }

    fn present(&mut self, conn: u32, pr: st::Present) -> Result<(), (FErr, &'static str)> {
        self.own(conn, pr.window_id)?;
        let w = self.desk.windows.get(pr.window_id).ok_or((FErr::NotFound, "no such window"))?;
        let s = self.desk.windows.surface_rect(w);
        let rects: Vec<Rect> = if pr.damage.is_empty() {
            alloc::vec![s]
        } else {
            pr.damage.iter().map(|d| Rect::new(s.x + d.x, s.y + d.y, d.w, d.h).intersect(&s)).collect()
        };
        for r in rects {
            self.damage(r);
        }
        Ok(())
    }

    fn close(&mut self, id: u32) {
        let mut out = Vec::new();
        self.desk.close(id, &mut out);
        if let Some(i) = self.wins.iter().position(|w| w.id == id) {
            let w = self.wins.remove(i);
            let _ = fstd::sys::call1(
                fcore_abi::syscall::VM_UNMAP,
                [well_known::SELF_ASPACE as u64, w.map.0, w.map.1, 0, 0, 0],
            );
        }
        self.apply(out);
    }

    fn set_theme(&mut self, t: st::Theme) {
        self.theme.mode = if t == st::Theme::Light { Mode::Light } else { Mode::Dark };
        self.damage(self.back.rect());
        for w in &self.wins {
            if let Ok(e) = fwire::event(st::EV_THEME, &st::SetTheme { theme: t }) {
                let _ = w.events.send(&e, Vec::new());
            }
        }
    }

    fn set_scale(&mut self, pct: u16) {
        self.theme.scale = pct as f32 / 100.0;
        self.desk.windows.set_strip(self.theme.px(fui::tokens_os::TITLE_STRIP_H));
        self.damage(self.back.rect());
        for w in &self.wins {
            if let Ok(e) = fwire::event(st::EV_SCALE, &st::SetScale { pct }) {
                let _ = w.events.send(&e, Vec::new());
            }
        }
        self.point();
    }

    /// The screen as it shows, cursor and all (or one window's frame of it),
    /// in a VMO of its own.
    fn screenshot(&self, window: Option<u32>) -> Reply {
        let r = match window {
            Some(id) => self.desk.windows.get(id).ok_or((FErr::NotFound, "no such window"))?.frame,
            None => self.back.rect(),
        }
        .intersect(&self.back.rect());
        let mut px: Vec<u32> = Vec::with_capacity((r.w * r.h) as usize);
        for y in r.y..r.bottom() {
            let at = (y as u32 * self.back.w + r.x as u32) as usize;
            px.extend_from_slice(&self.back.px[at..at + r.w as usize]);
        }
        let mut s = fimage::blit::Surface { px: &mut px, width: r.w, height: r.h, stride: r.w };
        cursor::draw(&self.cursor, &mut s, (r.x, r.y), self.inks());
        let bytes: Vec<u8> = px.iter().flat_map(|p| p.to_le_bytes()).collect();
        let vmo = fstd::sys::vmo_create(bytes.len().max(1) as u64)
            .ok()
            .and_then(Handle::from_raw)
            .ok_or((FErr::NoMemory, "no room for the picture"))?;
        fstd::sys::vmo_write(vmo.raw(), 0, &bytes).map_err(|_| (FErr::NoMemory, "no room for the picture"))?;
        let theirs =
            vmo.duplicate(rights::READ | rights::MAP | rights::TRANSFER).map_err(|_| (FErr::NoMemory, "no handle"))?;
        Ok((fwire::to_payload(&st::Shot { vmo: HandleRef(0), w: r.w, h: r.h }), alloc::vec![theirs]))
    }

    /// A window's published tree, as published (at most 60 KiB: a message's room).
    #[cfg(feature = "flavour-e2e")]
    fn ax_bytes(&self, window_id: u32) -> st::AxBytes {
        let mut bytes = Vec::new();
        if let Some((vmo, len)) = self.wins.iter().find(|w| w.id == window_id).and_then(|w| w.ax.as_ref())
            && *len <= 60 * 1024
        {
            bytes = alloc::vec![0u8; *len as usize];
            if fstd::sys::vmo_read(vmo.raw(), 8, &mut bytes).is_err() {
                bytes.clear();
            }
        }
        st::AxBytes { window_id, bytes }
    }

    #[cfg(any(feature = "flavour-e2e", feature = "flavour-dev"))]
    fn list(&self) -> st::Windows {
        let focused = self.desk.windows.focused();
        let windows = self
            .desk
            .windows
            .stack()
            .iter()
            .enumerate()
            .map(|(z, w)| st::WindowInfo {
                window_id: w.id,
                title: w.title.clone(),
                kind: w.kind,
                frame: st::Rect { x: w.frame.x, y: w.frame.y, w: w.frame.w, h: w.frame.h },
                focused: focused == Some(w.id),
                z: z as u32,
                owner: w.owner.clone(),
                ax_bytes: self.wins.iter().find(|x| x.id == w.id).and_then(|x| x.ax.as_ref()).map_or(0, |a| a.1),
                surface: {
                    let s = self.desk.windows.surface_rect(w);
                    st::Rect { x: s.x, y: s.y, w: s.w, h: s.h }
                },
            })
            .collect();
        let theme = if self.theme.mode == Mode::Light { st::Theme::Light } else { st::Theme::Dark };
        st::Windows { windows, theme, scale_pct: (self.theme.scale * 100.0 + 0.5) as u16 }
    }

    /// One message from drv-i8042.
    fn input(&mut self, bytes: &[u8]) {
        let Ok((h, p)) = fwire::parse(bytes) else { return };
        match h.ordinal {
            inp::KEY => {
                if let Ok(k) = fwire::from_payload::<inp::Key>(p) {
                    self.key(k);
                }
            }
            inp::MOUSE => {
                if let Ok(m) = fwire::from_payload::<inp::Mouse>(p) {
                    self.mouse(m);
                }
            }
            _ => {}
        }
    }

    /// A key, from drv-i8042 or injected.
    fn key(&mut self, k: inp::Key) {
        if self.idle.input(Instant::now().as_ns()) {
            self.saver(false);
            return;
        }
        let mut out = Vec::new();
        let key = st::Key { usage: k.usage, down: k.down, ch: k.ch, modifiers: k.modifiers };
        self.desk.key(key, &mut out);
        self.apply(out);
    }

    /// A mouse report, from drv-i8042 or injected.
    fn mouse(&mut self, m: inp::Mouse) {
        if self.idle.input(Instant::now().as_ns()) {
            self.saver(false);
            return;
        }
        let mut out = Vec::new();
        let was = self.desk.buttons;
        self.desk.mouse(m.dx as i32, m.dy as i32, m.wheel as i32, m.buttons, &mut out);
        if (was ^ m.buttons) & 1 != 0 {
            self.cursor.press(m.buttons & 1 != 0, Instant::now().as_ns());
        }
        self.point();
        self.apply(out);
    }

    /// A screensaver starts or ends: the desktop draws it (or takes it
    /// away), and the cursor hides while it's up.
    fn saver(&mut self, start: bool) {
        let (kind, reduce_motion) = (self.idle.kind, self.idle.reduce_motion);
        if start {
            fstd::info!("stage: the screensaver starts ({kind}{})", if reduce_motion { ", still" } else { "" });
        } else {
            fstd::info!("stage: the screensaver ends");
        }
        if let Some(ch) = &self.shortcuts
            && let Ok(e) = fwire::event(st::EV_SAVER, &st::SaverEvent { start, kind, reduce_motion })
        {
            let _ = ch.send(&e, Vec::new());
        }
        self.refresh.push(self.cursor.bounds());
    }

    fn apply(&mut self, out: Vec<Effect>) {
        for e in out {
            match e {
                Effect::Damage(r) => self.damage(r),
                Effect::Send(id, ev) => {
                    let Some(w) = self.wins.iter().find(|w| w.id == id) else { continue };
                    let msg = match ev {
                        Event::Key(k) => fwire::event(st::EV_KEY, &k),
                        Event::Pointer(p) => fwire::event(st::EV_POINTER, &p),
                        Event::Focus(f) => fwire::event(st::EV_FOCUS, &st::Focus { focused: f }),
                        Event::CloseRequested => fwire::event(st::EV_CLOSE_REQUESTED, &()),
                    };
                    // A client that's stopped reading loses events, never stage's time.
                    if let Ok(m) = msg {
                        let _ = w.events.send(&m, Vec::new());
                    }
                }
                Effect::Action(a) => {
                    use svc_stage::shortcuts::Action;
                    let s = match a {
                        Action::Launcher => Some(st::Shortcut::Launcher),
                        Action::Monitor => Some(st::Shortcut::Monitor),
                        Action::Terminal => Some(st::Shortcut::Terminal),
                        _ => None,
                    };
                    fstd::info!("stage: shortcut {a:?}");
                    if let (Some(s), Some(ch)) = (s, &self.shortcuts)
                        && let Ok(e) = fwire::event(st::EV_SHORTCUT, &st::ShortcutEvent { shortcut: s })
                    {
                        let _ = ch.send(&e, Vec::new());
                    }
                }
            }
        }
    }

    /// The cursor follows the pointer, and takes the look of what's under
    /// it: a window's chosen kind over its surface, the hover over the
    /// close button or a window's Hand, the ring elsewhere.
    fn point(&mut self) {
        let before = self.cursor.bounds();
        self.cursor.move_to(self.desk.x, self.desk.y);
        let (kind, hover) = match self.desk.windows.hit(self.desk.x, self.desk.y) {
            Some((_, Part::Close)) => (CursorKind::Hand, true),
            Some((id, Part::Surface { .. })) => {
                let k = self.wins.iter().find(|w| w.id == id).map_or(CursorKind::Arrow, |w| w.cursor);
                (k, k == CursorKind::Hand)
            }
            _ => (CursorKind::Arrow, false),
        };
        self.cursor.kind = kind;
        self.cursor.hover = hover;
        self.refresh.push(before);
        self.refresh.push(self.cursor.bounds());
    }

    /// One frame of the cursor's motion, if it's moving.
    fn animate(&mut self) {
        let now = Instant::now().as_ns();
        if !self.cursor.moving(now) {
            return;
        }
        let before = self.cursor.bounds();
        self.cursor.tick(now);
        self.refresh.push(before);
        self.refresh.push(self.cursor.bounds());
    }

    fn inks(&self) -> Inks {
        let c = |t| self.theme.colour(t);
        use fui::theme::Tok;
        Inks { ring: c(Tok::PrimaryInk), glyph: c(Tok::Text), edge: c(Tok::Bg), stop: c(Tok::Error) }
    }

    /// Puts `r` of the back buffer on the screen, the cursor drawn over it.
    fn show(&self, r: Rect) {
        let r = r.intersect(&self.back.rect());
        if r.is_empty() {
            return;
        }
        // SAFETY: the framebuffer stays mapped; it's h rows of `stride` pixels.
        unsafe { compose::present(&self.back, r, self.screen.px, self.screen.stride, self.screen.format) };
        if self.idle.on {
            return;
        }
        let o = r.intersect(&self.cursor.bounds());
        if o.is_empty() {
            return;
        }
        let mut px: Vec<u32> = Vec::with_capacity((o.w * o.h) as usize);
        for y in o.y..o.bottom() {
            let at = (y as u32 * self.back.w + o.x as u32) as usize;
            px.extend_from_slice(&self.back.px[at..at + o.w as usize]);
        }
        let mut s = fimage::blit::Surface { px: &mut px, width: o.w, height: o.h, stride: o.w };
        cursor::draw(&self.cursor, &mut s, (o.x, o.y), self.inks());
        let patch = Back { w: o.w, h: o.h, px };
        // SAFETY: as above; `o` lies inside the screen.
        unsafe { compose::present_at(&patch, (o.x, o.y), self.screen.px, self.screen.stride, self.screen.format) };
    }

    fn damage(&mut self, r: Rect) {
        let r = r.intersect(&self.back.rect());
        if r.is_empty() {
            return;
        }
        if self.damage.len() >= MAX_DAMAGE {
            let all = self.damage.drain(..).fold(r, |a, b| a.union(&b));
            self.damage.push(all);
        } else {
            self.damage.push(r);
        }
    }

    /// Composes the damage and puts it on the screen; the first time, S3.
    fn flush(&mut self) {
        if self.damage.is_empty() && self.refresh.is_empty() {
            return;
        }
        let damage = core::mem::take(&mut self.damage);
        {
            let wins = &self.wins;
            let surface = |id: u32| wins.iter().find(|w| w.id == id).map(|w| &w.pixels as &dyn Pixels);
            let mode = self.theme.mode;
            let art = self.art.as_mut().and_then(|a| a.for_theme(mode, self.screen.w, self.screen.h));
            let mut look = Look { theme: self.theme, fonts: &mut self.fonts, art };
            for r in &damage {
                compose::compose(&mut self.back, *r, &self.desk.windows, &surface, &mut look);
            }
        }
        // What changed, and where the cursor was and is: on the screen, cursor on top.
        let mut shown = damage;
        shown.append(&mut self.refresh);
        if shown.len() > MAX_DAMAGE {
            let all = shown.drain(..).fold(Rect::default(), |a, b| a.union(&b));
            shown.push(all);
        }
        for r in shown {
            self.show(r);
        }
        if !self.stamped {
            self.stamped = true;
            let at = rdtsc();
            if let Err(e) = fstd::service::stamp(13, at) {
                fstd::warn!("stage: S3 not stamped: {e:?}");
            }
            fstd::info!("stage: first frame, {}x{}", self.screen.w, self.screen.h);
        }
    }
}
