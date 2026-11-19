// SPDX-License-Identifier: Apache-2.0
//! Freally Terminal, the app (TASK-060, FR-032): a window of stage's showing
//! the grid in Noto Sans Mono 14 px, with Freally Shell inside it (TASK-061).
//!
//! - Keys become the bytes xterm sends (arrows as `ESC [ A`, or `ESC O A`
//!   when the program asks); the shell reads them and answers in VT bytes.
//! - The wheel and Shift+Page Up/Down scroll through the 10,000-line
//!   history; any key comes back to the live screen.
//! - Dragging selects (when the program isn't reading the mouse);
//!   Ctrl+Shift+C copies, Ctrl+Shift+V pastes (bracketed when asked).
//! - In e2e images, conductor's test door may feed recorded sequences,
//!   type, and read the screen back (`@terminal.vt`, `@shell.builtins`).
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use app_shell::builtins::{self, Fault, Follow, Record, Service, System};
use app_shell::line::{Editor, Event as LineEvent};
use app_terminal::grid::Grid;
use app_terminal::render::{self, Palette, Selection, View};
use fcore_abi::{FErr, perm, signal, topic, well_known};
use ffont::atlas::Target;
use ffont::text::{Cell, Face};
use fstd::handle::Handle;
use fstd::ipc::{Channel, Client};
use fstd::time::{Duration, Instant};
use fui::theme::{Mode, Theme};
use fwire::proto::ax;
use fwire::proto::conductor as c;
use fwire::proto::logger as lg;
use fwire::proto::stage as st;
use fwire::proto::terminal as term;

fstd::program!(run);

const COLS: u32 = 96;
const ROWS: u32 = 28;
const PAD: u32 = 8;
const PROMPT: &str = "\x1b[32mfreally\x1b[0m:~$ ";
/// How often `log -f` asks for new lines.
const FOLLOW_MS: u64 = 300;

fn deadline() -> u64 {
    Instant::after(Duration::from_secs(2))
}

/// What the shell's built-ins reach, for real.
struct Sys {
    stage: Option<Client>,
    logger: Option<Client>,
}

/// The boot's stages, by id (the protocol's own list).
const STAGES: [&str; 15] = c::STAGES;

impl System for Sys {
    fn services(&mut self) -> Result<Vec<Service>, String> {
        let s = fstd::service::ask::<_, c::Services>(c::LIST_SERVICES, &()).map_err(|e| alloc::format!("ps: {e:?}"))?;
        Ok(s.services
            .into_iter()
            .map(|v| Service {
                name: v.name,
                description: v.description,
                state: alloc::format!("{:?}", v.state),
                pid: v.pid,
                restarts: v.restarts,
            })
            .collect())
    }

    fn boot(&mut self) -> Result<Vec<(String, f64)>, String> {
        let t = fstd::service::ask::<_, c::BootTimeline>(c::BOOT_TIMELINE, &())
            .map_err(|e| alloc::format!("boot: {e:?}"))?;
        Ok(t.stages
            .into_iter()
            .map(|(id, ns)| (STAGES.get(id as usize).copied().unwrap_or("?").to_string(), ns as f64 / 1e6))
            .collect())
    }

    fn log(&mut self, service: Option<&str>, cursor: u64, limit: u16) -> Result<(Vec<Record>, u64), String> {
        if self.logger.is_none() {
            self.logger = fstd::service::connect("logger").ok().map(Client::new);
        }
        let l = self.logger.as_mut().ok_or("log: the logger isn't there")?;
        let req = lg::Read { cursor, limit, filter: lg::Filter { svc: service.map(String::from), min_level: None } };
        let (r, _) = l
            .call::<_, lg::Records>(lg::READ, &req, Vec::new(), deadline())
            .map_err(|e| alloc::format!("log: {e:?}"))?;
        let records = r
            .records
            .into_iter()
            .map(|x| Record { ms: x.ts_ns as f64 / 1e6, service: x.svc, level: x.level, msg: x.msg })
            .collect();
        Ok((records, r.next_cursor))
    }

    fn mem(&mut self) -> Result<(u64, u64), String> {
        let mut b = [0u8; 16];
        fstd::sys::system_info(topic::MEMORY, &mut b).map_err(|e| alloc::format!("mem: {}", e.describe()))?;
        let free = u64::from_le_bytes(b[..8].try_into().unwrap_or_default());
        let total = u64::from_le_bytes(b[8..].try_into().unwrap_or_default());
        Ok((total, total.saturating_sub(free)))
    }

    fn uptime_ns(&mut self) -> u64 {
        Instant::now().as_ns()
    }

    fn theme(&mut self, dark: bool) -> Result<(), String> {
        let t = if dark { st::Theme::Dark } else { st::Theme::Light };
        self.stage_call(st::SET_THEME, &st::SetTheme { theme: t })
    }

    fn scale(&mut self, pct: u16) -> Result<(), String> {
        self.stage_call(st::SET_SCALE, &st::SetScale { pct })
    }

    fn reboot(&mut self, _other: bool) -> Result<(), String> {
        Err(String::from("reboot: not yet. Freally OS can't restart the computer from inside it yet."))
    }

    fn faults(&mut self) -> Result<Vec<Fault>, String> {
        let f = fstd::service::ask::<_, c::Faults>(c::FAULTS, &c::Page { cursor: 0, limit: 32 })
            .map_err(|e| alloc::format!("faults: {e:?}"))?;
        Ok(f.reports
            .into_iter()
            .map(|r| Fault { seq: r.seq, service: r.service, kind: alloc::format!("{:?}", r.kind), message: r.message })
            .collect())
    }

    fn version(&self) -> String {
        let flavour = ["release", "dev", "e2e"].get(fstd::service::flavour() as usize).copied().unwrap_or("?");
        alloc::format!("Freally OS {} ({flavour})", fcore_abi::VERSION)
    }

    fn restart(&mut self, service: &str) -> Result<(), String> {
        fstd::service::ask::<_, ()>(c::RESTART, &c::ByName { name: String::from(service) })
            .map_err(|e| alloc::format!("restart: {e:?}"))
    }

    fn dev(&self) -> bool {
        fstd::service::flavour() == 1
    }

    fn list(&mut self, path: &str) -> Result<Vec<(String, bool)>, String> {
        fstd::fs::read_dir(path).map_err(|e| e.describe())
    }

    fn read_file(&mut self, path: &str) -> Result<Vec<u8>, String> {
        fstd::fs::read(path).map_err(|e| e.describe())
    }

    fn write_file(&mut self, path: &str, bytes: &[u8]) -> Result<(), String> {
        fstd::fs::write(path, bytes).map_err(|e| e.describe())
    }

    fn make_dir(&mut self, path: &str) -> Result<(), String> {
        fstd::fs::create_dir(path).map_err(|e| e.describe())
    }

    fn remove(&mut self, path: &str) -> Result<(), String> {
        fstd::fs::remove(path).map_err(|e| e.describe())
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        fstd::fs::rename(from, to).map_err(|e| e.describe())
    }

    fn whoami(&mut self) -> Result<String, String> {
        fstd::fs::whoami().map_err(|e| e.describe())
    }
}

impl Sys {
    fn stage_call(&mut self, ordinal: u32, req: &impl fwire::Wire) -> Result<(), String> {
        let s = self.stage.as_mut().ok_or("stage isn't there")?;
        s.call::<_, ()>(ordinal, req, Vec::new(), deadline()).map(|_| ()).map_err(|e| alloc::format!("{e:?}"))
    }
}

/// The window's surface as a text target.
struct Surface {
    px: *mut u32,
    w: u32,
    h: u32,
}

impl Target for Surface {
    fn size(&self) -> (usize, usize) {
        (self.w as usize, self.h as usize)
    }
    fn get(&self, x: usize, y: usize) -> u32 {
        // SAFETY: inside the w × h surface, mapped for the window's life.
        unsafe { *self.px.add(y * self.w as usize + x) & 0xFF_FFFF }
    }
    fn set(&mut self, x: usize, y: usize, rgb: u32) {
        // SAFETY: as above; every pixel opaque.
        unsafe { *self.px.add(y * self.w as usize + x) = 0xFF00_0000 | rgb };
    }
}

struct Term {
    stage: Client,
    window: u32,
    events: Channel,
    surface: Surface,
    _vmo: Handle,
    faces: Vec<Face<'static>>,
    px: f32,
    cell: Cell,
    theme: Theme,
    grid: Grid,
    editor: Editor,
    sys: Sys,
    follow: Option<Follow>,
    /// How far back into the history the view is.
    back: usize,
    selection: Option<Selection>,
    selecting: bool,
    clipboard: String,
    focused: bool,
    shift: bool,
    ax: fstd::ax::Publisher,
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    match Term::open() {
        Ok(mut t) => t.serve(),
        Err(e) => {
            fstd::error!("terminal: {e}");
            1
        }
    }
}

impl Term {
    fn open() -> Result<Term, String> {
        let mono = fstd::service::font("NotoSansMono-Regular.ttf")
            .map_err(|e| alloc::format!("the font: {}", e.describe()))?;
        let symbols = fstd::service::font("NotoSansSymbols2-Regular.ttf").ok();
        let mut faces = alloc::vec![Face::new(mono).map_err(|e| alloc::format!("{e:?}"))?];
        if let Some(s) = symbols.and_then(|s| Face::new(s).ok()) {
            faces.push(s);
        }
        let ch = fstd::service::connect("stage").map_err(|e| alloc::format!("stage: {e:?}"))?;
        let mut stage = Client::new(ch);
        let (scr, _) = stage
            .call::<_, st::ScreenInfo>(st::SCREEN, &(), Vec::new(), deadline())
            .map_err(|e| alloc::format!("the screen: {e:?}"))?;
        let theme = Theme::new(
            if scr.theme == st::Theme::Light { Mode::Light } else { Mode::Dark },
            scr.scale_pct as f32 / 100.0,
        );
        let px = 14.0 * theme.scale;
        let cell = ffont::text::cell(&mut faces[0], px);
        let pad = theme.px(PAD);
        let (w, h) = (COLS * cell.w as u32 + 2 * pad, ROWS * cell.h as u32 + 2 * pad);
        let req = st::CreateWindow {
            title: String::from("Terminal"),
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
        let sys_stage = fstd::service::connect("stage").ok().map(Client::new);
        let mut t = Term {
            stage,
            window: made.window_id,
            events,
            surface: Surface { px: at as *mut u32, w, h },
            _vmo: vmo,
            faces,
            px,
            cell,
            theme,
            grid: Grid::new(COLS as usize, ROWS as usize),
            editor: Editor::new(),
            sys: Sys { stage: sys_stage, logger: None },
            follow: None,
            back: 0,
            selection: None,
            selecting: false,
            clipboard: String::new(),
            focused: true,
            ax: Default::default(),
            shift: false,
        };
        t.grid.feed(b"Freally Shell. Type help to see the commands.\r\n");
        t.grid.feed(PROMPT.as_bytes());
        t.draw();
        fstd::info!("terminal: open, {COLS}x{ROWS}");
        Ok(t)
    }

    fn draw(&mut self) {
        let p = Palette::of(&self.theme);
        let (w, h) = (self.surface.w, self.surface.h);
        for i in 0..(w * h) as usize {
            // SAFETY: inside the w × h surface.
            unsafe { *self.surface.px.add(i) = 0xFF00_0000 | p.bg };
        }
        let pad = self.theme.px(PAD) as i32;
        let view = View { grid: &self.grid, back: self.back, selection: self.selection, focused: self.focused };
        render::render(&mut self.surface, &mut self.faces, self.px, self.cell, (pad, pad), &view, &p);
        let _ = self.stage.call::<_, ()>(
            st::PRESENT,
            &st::Present { window_id: self.window, damage: Vec::new() },
            Vec::new(),
            deadline(),
        );
        let _ = self.stage.call::<_, ()>(
            st::SET_TITLE,
            &st::SetTitle {
                window_id: self.window,
                title: if self.grid.title.is_empty() { String::from("Terminal") } else { self.grid.title.clone() },
            },
            Vec::new(),
            deadline(),
        );
        // Its tree: the screen, a text box whose value is the cursor's line.
        let pad = self.theme.px(PAD) as i32;
        let (cw, ch) = (self.grid.cols as i32 * self.cell.w, self.grid.rows as i32 * self.cell.h);
        let state = if self.focused { ax::FOCUSED } else { 0 };
        let mut n =
            fstd::ax::node(ax::ROOT, ax::TEXTBOX, "Terminal", state, (pad, pad, cw.max(0) as u32, ch.max(0) as u32));
        n.testid = String::from("terminal-screen");
        n.value = String::from(self.grid.text(self.grid.y).trim_end());
        self.ax.publish(&mut self.stage, self.window, &ax::Tree { nodes: alloc::vec![n] });
    }

    /// Text from the shell (its `\n` made `\r\n`) to the screen.
    fn say(&mut self, text: &str) {
        let mut s = String::with_capacity(text.len() + 8);
        for ch in text.chars() {
            if ch == '\n' {
                s.push('\r');
            }
            s.push(ch);
        }
        self.grid.feed(s.as_bytes());
    }

    /// Bytes as the shell reads them (typed, or pasted).
    fn input(&mut self, bytes: &[u8]) {
        if self.follow.is_some() {
            // Ctrl+C ends `log -f`; anything else waits.
            if bytes.contains(&0x03) {
                self.follow = None;
                self.say("^C\n");
                self.grid.feed(PROMPT.as_bytes());
            }
            return;
        }
        for e in self.editor.feed(bytes) {
            match e {
                LineEvent::Nothing => {}
                LineEvent::Redraw => {
                    let d = self.editor.draw(PROMPT);
                    self.grid.feed(d.as_bytes());
                }
                LineEvent::Cancel => {
                    self.say("^C\n");
                    self.grid.feed(PROMPT.as_bytes());
                }
                LineEvent::Clear => {
                    self.grid.feed(b"\x1b[2J\x1b[H");
                    let d = self.editor.draw(PROMPT);
                    self.grid.feed(d.as_bytes());
                }
                LineEvent::Line(line) => {
                    self.say("\n");
                    self.command(&line);
                    if self.follow.is_none() {
                        self.grid.feed(PROMPT.as_bytes());
                    }
                }
            }
        }
    }

    fn command(&mut self, line: &str) {
        match builtins::parse(line, self.sys.dev()) {
            Ok(None) => {}
            Ok(Some(cmd)) => {
                let out = builtins::run(&cmd, &mut self.sys);
                if out.clear {
                    self.grid.feed(b"\x1b[2J\x1b[3J\x1b[H");
                }
                self.say(&out.text);
                self.follow = out.follow;
                // The theme or the scale may have changed: stage tells the window.
            }
            Err(e) => {
                self.say(&e);
                self.say("\n");
            }
        }
    }

    /// `log -f`: new lines since the last look.
    fn follow_log(&mut self) {
        let Some(f) = self.follow.clone() else { return };
        if let Ok((records, next)) = self.sys.log(f.service.as_deref(), f.cursor, 200)
            && !records.is_empty()
        {
            let text: String = records.iter().map(builtins::record).collect();
            self.say(&text);
            self.follow = Some(Follow { service: f.service, cursor: next });
            self.draw();
        }
    }

    /// A key as xterm sends it.
    fn key(&mut self, k: st::Key) {
        const SHIFT: u8 = 1;
        const CTRL: u8 = 2;
        if matches!(k.usage, 0xE1 | 0xE5) {
            self.shift = k.down;
        }
        if !k.down {
            return;
        }
        let (ctrl, shift) = (k.modifiers & CTRL != 0, k.modifiers & SHIFT != 0);
        // Ctrl+Shift+C and V: copy and paste.
        if ctrl && shift && k.usage == 0x06 {
            if let Some(s) = self.selection {
                self.clipboard = render::selected_text(&self.grid, s);
            }
            return;
        }
        if ctrl && shift && k.usage == 0x19 {
            let mut bytes = Vec::new();
            if self.grid.modes.bracketed_paste {
                bytes.extend_from_slice(b"\x1b[200~");
            }
            bytes.extend(self.clipboard.replace('\n', "\r").bytes());
            if self.grid.modes.bracketed_paste {
                bytes.extend_from_slice(b"\x1b[201~");
            }
            self.back = 0;
            self.input(&bytes);
            self.draw();
            return;
        }
        // Shift+Page Up/Down: the history.
        if shift && matches!(k.usage, 0x4B | 0x4E) {
            let page = self.grid.rows - 1;
            self.back = if k.usage == 0x4B {
                (self.back + page).min(self.grid.scrollback.len())
            } else {
                self.back.saturating_sub(page)
            };
            self.draw();
            return;
        }
        let app = self.grid.modes.app_cursor;
        let arrow = |c: u8| -> Vec<u8> { if app { alloc::vec![0x1B, b'O', c] } else { alloc::vec![0x1B, b'[', c] } };
        let bytes: Vec<u8> = match k.usage {
            0x28 => alloc::vec![b'\r'],
            0x29 => alloc::vec![0x1B],
            0x2A => alloc::vec![0x7F],
            0x2B => alloc::vec![b'\t'],
            0x4F => arrow(b'C'),
            0x50 => arrow(b'D'),
            0x51 => arrow(b'B'),
            0x52 => arrow(b'A'),
            0x4A => b"\x1b[H".to_vec(),
            0x4D => b"\x1b[F".to_vec(),
            0x4C => b"\x1b[3~".to_vec(),
            0x4B => b"\x1b[5~".to_vec(),
            0x4E => b"\x1b[6~".to_vec(),
            _ => match char::from_u32(k.ch).filter(|c| *c != '\0') {
                Some(c) if ctrl && c.is_ascii_alphabetic() => alloc::vec![(c.to_ascii_lowercase() as u8) & 0x1F],
                Some(c) => {
                    let mut b = [0u8; 4];
                    c.encode_utf8(&mut b).as_bytes().to_vec()
                }
                None => return,
            },
        };
        self.back = 0;
        self.selection = None;
        self.input(&bytes);
        self.draw();
    }

    /// The cell under a point of the surface, as (col, history row).
    fn cell_at(&self, x: i32, y: i32) -> (usize, usize) {
        let pad = self.theme.px(PAD) as i32;
        let col = ((x - pad).max(0) / self.cell.w) as usize;
        let row = ((y - pad).max(0) / self.cell.h) as usize;
        (col.min(self.grid.cols - 1), render::history_row(&self.grid, self.back, row.min(self.grid.rows - 1)))
    }

    fn pointer(&mut self, p: st::Pointer) {
        if self.grid.modes.mouse != 0 && self.grid.modes.mouse_sgr && p.kind != st::PointerKind::Wheel {
            // The program reads the mouse: SGR reports.
            let (col, row) = self.cell_at(p.x, p.y);
            let screen_row = row.saturating_sub(self.grid.scrollback.len());
            let (b, end) = match p.kind {
                st::PointerKind::Down => (0, 'M'),
                st::PointerKind::Up => (0, 'm'),
                st::PointerKind::Move if p.buttons & 1 != 0 && self.grid.modes.mouse >= 1002 => (32, 'M'),
                st::PointerKind::Move if self.grid.modes.mouse == 1003 => (35, 'M'),
                _ => return,
            };
            let r = alloc::format!("\x1b[<{b};{};{}{end}", col + 1, screen_row + 1);
            self.grid.replies.extend_from_slice(r.as_bytes());
            return;
        }
        match p.kind {
            st::PointerKind::Wheel => {
                let n = 3usize;
                self.back = if p.dy > 0 {
                    (self.back + n).min(self.grid.scrollback.len())
                } else {
                    self.back.saturating_sub(n)
                };
                self.draw();
            }
            st::PointerKind::Down if p.buttons & 1 != 0 => {
                let at = self.cell_at(p.x, p.y);
                self.selection = Some(Selection { from: at, to: at });
                self.selecting = true;
                self.draw();
            }
            st::PointerKind::Move if self.selecting => {
                let at = self.cell_at(p.x, p.y);
                if let Some(s) = &mut self.selection {
                    s.to = at;
                }
                self.draw();
            }
            st::PointerKind::Up => {
                self.selecting = false;
                if self.selection.is_some_and(|s| s.from == s.to) {
                    self.selection = None;
                    self.draw();
                }
            }
            _ => {}
        }
    }

    fn window_event(&mut self, ordinal: u32, p: &[u8]) -> bool {
        match ordinal {
            st::EV_KEY => {
                if let Ok(k) = fwire::from_payload::<st::Key>(p) {
                    self.key(k);
                }
            }
            st::EV_POINTER => {
                if let Ok(e) = fwire::from_payload::<st::Pointer>(p) {
                    self.pointer(e);
                }
            }
            st::EV_FOCUS => {
                if let Ok(f) = fwire::from_payload::<st::Focus>(p) {
                    self.focused = f.focused;
                    self.draw();
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

    /// The e2e door's requests (conductor only). False once the client's gone.
    fn door(&mut self, ch: &Channel, name: &str) -> bool {
        let mut typed = Vec::new();
        let alive = self.door_requests(ch, name, &mut typed);
        if !typed.is_empty() {
            self.input(&typed);
            self.draw();
        }
        alive
    }

    fn door_requests(&mut self, ch: &Channel, name: &str, typed: &mut Vec<u8>) -> bool {
        loop {
            let m = match ch.try_recv() {
                Ok(m) => m,
                Err(FErr::ShouldWait) => return true,
                Err(_) => return false,
            };
            let Ok((h, p)) = fwire::parse(&m.bytes) else { continue };
            let reply = if name != "conductor" || fstd::service::flavour() != 2 {
                fwire::error_reply(
                    h.txid,
                    h.ordinal,
                    FErr::AccessDenied as i32,
                    "the test door is conductor's, in e2e images",
                )
            } else {
                match h.ordinal {
                    term::FEED | term::TYPE => match fwire::from_payload::<term::Bytes>(p) {
                        Ok(b) => {
                            if h.ordinal == term::FEED {
                                self.grid.feed(&b.data);
                                self.draw();
                            } else {
                                // Typed bytes run after the answer: a command may ask
                                // conductor, which is waiting on this very call.
                                typed.extend_from_slice(&b.data);
                            }
                            fwire::reply(h.txid, h.ordinal, &())
                        }
                        Err(_) => fwire::error_reply(h.txid, h.ordinal, FErr::InvalidArgs as i32, "unreadable"),
                    },
                    term::SCREEN => fwire::reply(
                        h.txid,
                        h.ordinal,
                        &term::Screen {
                            rows: (0..self.grid.rows).map(|y| self.grid.text(y)).collect(),
                            cursor: (self.grid.x as u32, self.grid.y as u32),
                            scrollback: self.grid.scrollback.len() as u32,
                            cols: self.grid.cols as u32,
                            window: self.window,
                        },
                    ),
                    _ => fwire::error_reply(h.txid, h.ordinal, FErr::NotSupported as i32, "not served"),
                }
            };
            if let Ok(r) = reply {
                let _ = ch.send(&r, Vec::new());
            }
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
            let until = if self.follow.is_some() { Instant::after(Duration::from_millis(FOLLOW_MS)) } else { u64::MAX };
            match fstd::sys::wait_many(&items, until) {
                Ok(0) | Err(_) => {
                    let mut evs = Vec::new();
                    loop {
                        match self.events.try_recv() {
                            Ok(m) => {
                                if let Ok((h, p)) = fwire::parse(&m.bytes) {
                                    evs.push((h.ordinal, p.to_vec()));
                                }
                            }
                            Err(FErr::ShouldWait) | Err(FErr::TimedOut) => break,
                            Err(_) => return 0,
                        }
                    }
                    for (o, p) in evs {
                        if !self.window_event(o, &p) {
                            fstd::info!("terminal: closed");
                            return 0;
                        }
                    }
                }
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
            }
            self.follow_log();
            // What the program asked back (cursor reports, mouse) goes to the shell, which ignores it.
            self.grid.replies.clear();
        }
    }
}
