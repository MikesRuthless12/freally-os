// SPDX-License-Identifier: Apache-2.0
//! The test door's windows (e2e flavour only): conductor as one of stage's
//! clients, through stage's own protocol, so `@stage.windows` drives real
//! windows. Each is a solid colour; asked to close (its close button,
//! Alt+F4), it closes, as an app would.
//!
//! - `stage open <w> <h> <rrggbb> <title>` → `window <id>`;
//! - `stage close|focus <id>`, `stage title <id> <text>`, `stage theme dark|light`;
//! - `stage cursor <id> <kind>` (`set_cursor`, its number: 1 hand, 2 text…);
//! - `stage scale <pct>`; `stage ax <id> <bytes>` publishes a tree of that
//!   many bytes for the window; `stage shot` asks for a screenshot (the door
//!   isn't the Brain or cdpd: stage refuses it), and `stage inject` an empty
//!   injection (refused the same way);
//! - `stage list` → `window id=… x=… y=… w=… h=… focused=… z=… title=…` each,
//!   then `theme Dark|Light`;
//! - `stage pointer` → `pointer <x> <y> <buttons>`;
//! - `stage events` → what the windows heard since the last time:
//!   `event <id> focus true|false`, `event <id> close`, `event <id> key <usage> down|up`,
//!   `event <id> pointer Down|Up|Move|Wheel <x> <y> <buttons>`.

use crate::services::Conductor;
use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::{FErr, perm, well_known};
use fstd::handle::Handle;
use fstd::ipc::{Channel, Client};
use fstd::time::{Duration, Instant};
use fwire::proto::stage as st;

/// Events kept for `stage events`, the oldest dropped past this.
const MAX_EVENTS: usize = 512;

struct Win {
    id: u32,
    events: Channel,
    _vmo: Handle,
    map: (u64, u64),
}

#[derive(Default)]
pub struct StageDoor {
    client: Option<Client>,
    wins: Vec<Win>,
    events: Vec<String>,
}

fn deadline() -> u64 {
    Instant::after(Duration::from_secs(2))
}

impl StageDoor {
    pub fn run(&mut self, c: &Conductor, arg: &str) -> Vec<String> {
        if self.client.is_none() {
            self.client = c.open_connection("stage", "conductor").map(Client::new);
        }
        if self.client.is_none() {
            return alloc::vec![String::from("error: stage isn't up")];
        }
        self.hear();
        let (op, rest) = arg.split_once(' ').unwrap_or((arg, ""));
        let r = match op {
            "open" => self.open(rest),
            "close" => self.id(rest).and_then(|id| self.close(id)),
            "focus" => self.id(rest).and_then(|id| self.call::<()>(st::FOCUS_WINDOW, &st::ById { window_id: id })),
            "title" => {
                let (id, text) = rest.split_once(' ').unwrap_or((rest, ""));
                self.id(id).and_then(|id| {
                    self.call::<()>(st::SET_TITLE, &st::SetTitle { window_id: id, title: String::from(text) })
                })
            }
            "cursor" => {
                let (id, kind) = rest.split_once(' ').unwrap_or((rest, ""));
                match (self.id(id), kind.trim().parse::<u8>()) {
                    (Ok(id), Ok(kind)) => self.call::<()>(st::SET_CURSOR, &st::SetCursor { window_id: id, kind }),
                    _ => Err(String::from("stage cursor <id> <kind number>")),
                }
            }
            "scale" => match rest.trim().parse::<u16>() {
                Ok(pct) => self.call::<()>(st::SET_SCALE, &st::SetScale { pct }),
                Err(_) => Err(String::from("stage scale <pct>")),
            },
            "ax" => {
                let (id, n) = rest.split_once(' ').unwrap_or((rest, ""));
                match (self.id(id), n.trim().parse::<u64>()) {
                    (Ok(id), Ok(n)) if n <= 1 << 20 => self.ax(id, n),
                    _ => Err(String::from("stage ax <id> <bytes ≤ 1 MiB>")),
                }
            }
            "inject" => self.client().and_then(|cl| {
                cl.call::<_, ()>(st::INJECT_INPUT, &st::Inject { key: None, mouse: None }, Vec::new(), deadline())
                    .map(|_| alloc::vec![String::from("ok")])
                    .map_err(|e| alloc::format!("{e:?}"))
            }),
            "shot" => self.client().and_then(|cl| {
                cl.call::<_, st::Shot>(st::SCREENSHOT, &st::Screenshot { window_id: None }, Vec::new(), deadline())
                    .map(|(s, _)| alloc::vec![alloc::format!("shot {}x{}", s.w, s.h)])
                    .map_err(|e| alloc::format!("{e:?}"))
            }),
            "theme" => {
                let theme = match rest {
                    "dark" => Ok(st::Theme::Dark),
                    "light" => Ok(st::Theme::Light),
                    _ => Err(String::from("theme dark or theme light")),
                };
                theme.and_then(|t| self.call::<()>(st::SET_THEME, &st::SetTheme { theme: t }))
            }
            "list" => self.list(),
            "pointer" => self.client().and_then(|cl| {
                cl.call::<_, st::PointerAt>(st::POINTER, &(), Vec::new(), deadline())
                    .map(|(p, _)| alloc::vec![alloc::format!("pointer {} {} {}", p.x, p.y, p.buttons)])
                    .map_err(|e| alloc::format!("{e:?}"))
            }),
            "events" => Ok(core::mem::take(&mut self.events)),
            _ => Err(String::from("stage open|close|focus|title|cursor|scale|ax|shot|theme|list|pointer|events")),
        };
        r.unwrap_or_else(|e| alloc::vec![alloc::format!("error: {e}")])
    }

    fn client(&mut self) -> Result<&mut Client, String> {
        self.client.as_mut().ok_or_else(|| String::from("stage isn't up"))
    }

    fn call<R: fwire::Wire>(&mut self, ordinal: u32, req: &impl fwire::Wire) -> Result<Vec<String>, String> {
        let cl = self.client()?;
        cl.call::<_, R>(ordinal, req, Vec::new(), deadline()).map_err(|e| alloc::format!("{e:?}"))?;
        Ok(alloc::vec![String::from("ok")])
    }

    fn id(&self, s: &str) -> Result<u32, String> {
        s.trim().parse().map_err(|_| alloc::format!("{s:?} isn't a window id"))
    }

    fn open(&mut self, rest: &str) -> Result<Vec<String>, String> {
        let mut w = rest.splitn(4, ' ');
        let (Some(width), Some(height), Some(rgb), title) = (w.next(), w.next(), w.next(), w.next().unwrap_or(""))
        else {
            return Err(String::from("stage open <w> <h> <rrggbb> <title>"));
        };
        let (Ok(width), Ok(height), Ok(rgb)) =
            (width.parse::<u32>(), height.parse::<u32>(), u32::from_str_radix(rgb, 16))
        else {
            return Err(String::from("unreadable size or colour"));
        };
        let req = st::CreateWindow {
            title: String::from(title),
            w: width,
            h: height,
            min_w: 0,
            min_h: 0,
            kind: st::Kind::Normal,
            x: 0,
            y: 0,
        };
        let cl = self.client()?;
        let (made, mut hs) = cl
            .call::<_, st::Created>(st::CREATE_WINDOW, &req, Vec::new(), deadline())
            .map_err(|e| alloc::format!("{e:?}"))?;
        if hs.len() != 2 {
            return Err(String::from("stage sent no surface"));
        }
        let events = Channel(hs.remove(1));
        let vmo = hs.remove(0);
        let len = (width as u64 * height as u64 * 4).next_multiple_of(4096);
        let at = fstd::sys::vm_map(well_known::SELF_ASPACE, vmo.raw(), 0, len, 0, perm::READ | perm::WRITE)
            .map_err(|e| String::from(e.describe()))?;
        let px = at as *mut u32;
        for i in 0..(width * height) as usize {
            // SAFETY: inside the w × h pixels just mapped.
            unsafe { core::ptr::write_volatile(px.add(i), 0xFF00_0000 | (rgb & 0xFF_FFFF)) };
        }
        self.wins.push(Win { id: made.window_id, events, _vmo: vmo, map: (at, len) });
        self.call::<()>(st::PRESENT, &st::Present { window_id: made.window_id, damage: Vec::new() })?;
        Ok(alloc::vec![alloc::format!("window {}", made.window_id)])
    }

    /// Publishes an accessibility tree of `n` bytes (its length first) for window `id`.
    fn ax(&mut self, id: u32, n: u64) -> Result<Vec<String>, String> {
        let vmo = fstd::sys::vmo_create(n + 8).ok().and_then(Handle::from_raw).ok_or("no VMO")?;
        fstd::sys::vmo_write(vmo.raw(), 0, &n.to_le_bytes()).map_err(|e| String::from(e.describe()))?;
        let cl = self.client()?;
        cl.call::<_, ()>(
            st::PUBLISH_AX_TREE,
            &st::AxTree { window_id: id, tree: fwire::HandleRef(0) },
            alloc::vec![vmo],
            deadline(),
        )
        .map_err(|e| alloc::format!("{e:?}"))?;
        Ok(alloc::vec![String::from("ok")])
    }

    fn close(&mut self, id: u32) -> Result<Vec<String>, String> {
        let r = self.call::<()>(st::CLOSE_WINDOW, &st::ById { window_id: id });
        if let Some(i) = self.wins.iter().position(|w| w.id == id) {
            let w = self.wins.remove(i);
            let _ = fstd::sys::call1(
                fcore_abi::syscall::VM_UNMAP,
                [well_known::SELF_ASPACE as u64, w.map.0, w.map.1, 0, 0, 0],
            );
        }
        r
    }

    fn list(&mut self) -> Result<Vec<String>, String> {
        let cl = self.client()?;
        let (ws, _) = cl
            .call::<_, st::Windows>(st::LIST_WINDOWS, &(), Vec::new(), deadline())
            .map_err(|e| alloc::format!("{e:?}"))?;
        let mut out: Vec<String> = ws
            .windows
            .iter()
            .map(|w| {
                alloc::format!(
                    "window id={} x={} y={} w={} h={} focused={} z={} kind={:?} ax={} owner={} title={}",
                    w.window_id,
                    w.frame.x,
                    w.frame.y,
                    w.frame.w,
                    w.frame.h,
                    w.focused,
                    w.z,
                    w.kind,
                    w.ax_bytes,
                    w.owner,
                    w.title
                )
            })
            .collect();
        out.push(alloc::format!("theme {:?}", ws.theme));
        out.push(alloc::format!("scale {}", ws.scale_pct));
        Ok(out)
    }

    /// Reads what every window heard; one asked to close closes.
    fn hear(&mut self) {
        let mut close = Vec::new();
        for w in &self.wins {
            loop {
                let m = match w.events.try_recv() {
                    Ok(m) => m,
                    Err(FErr::ShouldWait) | Err(_) => break,
                };
                let Ok((h, p)) = fwire::parse(&m.bytes) else { continue };
                let line = match h.ordinal {
                    st::EV_FOCUS => fwire::from_payload::<st::Focus>(p)
                        .ok()
                        .map(|f| alloc::format!("event {} focus {}", w.id, f.focused)),
                    st::EV_CLOSE_REQUESTED => {
                        close.push(w.id);
                        Some(alloc::format!("event {} close", w.id))
                    }
                    st::EV_KEY => fwire::from_payload::<st::Key>(p).ok().map(|k| {
                        alloc::format!("event {} key {:#04x} {}", w.id, k.usage, if k.down { "down" } else { "up" })
                    }),
                    st::EV_POINTER => fwire::from_payload::<st::Pointer>(p)
                        .ok()
                        .map(|e| alloc::format!("event {} pointer {:?} {} {} {}", w.id, e.kind, e.x, e.y, e.buttons)),
                    st::EV_THEME => fwire::from_payload::<st::SetTheme>(p)
                        .ok()
                        .map(|t| alloc::format!("event {} theme {:?}", w.id, t.theme)),
                    st::EV_SCALE => fwire::from_payload::<st::SetScale>(p)
                        .ok()
                        .map(|s| alloc::format!("event {} scale {}", w.id, s.pct)),
                    _ => None,
                };
                if let Some(l) = line {
                    if self.events.len() >= MAX_EVENTS {
                        self.events.remove(0);
                    }
                    self.events.push(l);
                }
            }
        }
        for id in close {
            let _ = self.close(id);
        }
    }
}
