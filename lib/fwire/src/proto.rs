// SPDX-License-Identifier: Apache-2.0
//! The service protocols (PRD § 4): ordinals and message bodies.

/// Conductor, on every process's well-known handle 1 (the directory channel).
pub mod conductor {
    use crate::{HandleRef, wire_enum, wire_struct};
    use alloc::string::String;
    use alloc::vec::Vec;

    /// Conductor's first message to a service it starts (an event on handle 1).
    pub const START: u32 = 0;
    pub const CONNECT: u32 = 1;
    pub const REGISTER: u32 = 2;
    pub const LIST_SERVICES: u32 = 3;
    pub const RESTART: u32 = 4;
    pub const SWAP: u32 = 5;
    pub const ROLLBACK: u32 = 6;
    pub const FAULTS: u32 = 7;
    pub const BOOT_TIMELINE: u32 = 8;
    pub const WATCH: u32 = 9;
    /// To a service's registered endpoint: a client connected (an event; the
    /// new channel is handle 0).
    pub const INCOMING: u32 = 10;
    /// A boot stage the caller marks (PRD § 3): S3 from stage, S4 from desktop.
    pub const STAMP: u32 = 11;
    /// A service's manifest as the image carries it (`ByName` → `Text`;
    /// TASK-081: the Brain's `freally://services/{name}/manifest`).
    pub const MANIFEST: u32 = 12;
    /// Conductor to a service with `handoff = true`, on a connection of its
    /// own, during a swap (dev images; TASK-083): its state (`HandedOff`).
    pub const HANDOFF: u32 = 13;
    /// The Brain's `reboot` (dev images; TASK-084): `Reboot` → `()`, then the
    /// machine resets half a second later (the answer has time to leave).
    pub const REBOOT: u32 = 14;
    /// A data file one of the caller's grants allows, by name (`ByName` →
    /// `()`, the file's VMO handle 0 of the reply, read-only): `hardware:150`,
    /// `os:150`, `os:wallpaper` (TASK-090A: the art at nine scales, made
    /// into a VMO only when a service first asks).
    pub const DATA: u32 = 15;
    /// Shut down or Restart (`Power` → `()`; the login screen, the
    /// desktop): ffs commits clean, then the machine powers off or resets
    /// (TASK-102).
    pub const POWER: u32 = 16;

    wire_struct! {
        /// `start`: who the service is, the image's flavour (0 release, 1 dev,
        /// 2 e2e), and the handles its manifest's grants carry, by name.
        pub struct Start { pub name: String, pub flavour: u8, pub handles: Vec<(String, HandleRef)> }
    }

    wire_struct! {
        /// `incoming`: who connected; the channel is handle 0.
        pub struct Incoming { pub client: String, pub channel: HandleRef }
    }

    wire_struct! {
        /// `connect` and `restart` and `rollback`: a service by name.
        pub struct ByName { pub name: String }
    }
    wire_struct! {
        /// `manifest`'s answer.
        pub struct Text { pub text: String }
    }

    wire_struct! {
        /// `connect`'s reply: the channel, handle 0 of the reply.
        pub struct Connected { pub channel: HandleRef }
    }

    wire_struct! {
        pub struct Register { pub name: String, pub endpoint: HandleRef }
    }

    wire_enum! {
        pub enum ServiceState {
            Waiting = 0,
            Starting = 1,
            Running = 2,
            Restarting = 3,
            /// Crashed too often (5 times in 60 s): held stopped.
            Held = 4,
            Stopped = 5,
        }
    }

    wire_struct! {
        pub struct ServiceInfo {
            pub name: String,
            pub description: String,
            pub state: ServiceState,
            pub pid: u32,
            pub restarts: u32,
            pub started_ns: u64,
            pub binary_sha256: [u8; 32],
            /// CPU time its process has run (ns) and the bytes it has mapped.
            pub cpu_ns: u64,
            pub mem_bytes: u64,
        }
    }

    wire_struct! {
        pub struct Services { pub services: Vec<ServiceInfo> }
    }

    wire_struct! {
        /// `swap` (brain only, dev images only; TASK-083): the new build is
        /// handle 0, a VMO holding its length (u64, little-endian) and then the
        /// ELF; `signature` is the dev key's Ed25519 signature over its SHA-256.
        pub struct Swap { pub name: String, pub binary: HandleRef, pub signature: [u8; 64] }
    }

    wire_struct! {
        /// `ms`: from the call to the new build started, its state handed over.
        pub struct Swapped { pub old_sha256: [u8; 32], pub new_sha256: [u8; 32], pub ms: u32 }
    }

    wire_struct! {
        /// `reboot`: into the same slot, or the other.
        pub struct Reboot { pub other_slot: bool }
    }
    wire_struct! {
        /// `power`: off, or a restart.
        pub struct Power { pub off: bool }
    }

    wire_struct! {
        /// `rollback`'s answer: the build running again.
        pub struct RolledBack { pub restored_sha256: [u8; 32] }
    }

    wire_struct! {
        /// `handoff`'s answer (a service with `handoff = true`, dev images):
        /// its state, a VMO (handle 0) holding the length (u64, little-endian)
        /// and then the bytes. The new build finds it as the grant `handoff`.
        pub struct HandedOff { pub state: HandleRef }
    }

    wire_struct! {
        /// Paging: `cursor` 0 starts at the oldest; `next_cursor` 0 means the end.
        pub struct Page { pub cursor: u64, pub limit: u16 }
    }

    wire_enum! {
        pub enum FaultKind {
            Panic = 0,
            PageFault = 1,
            GeneralProtection = 2,
            InvalidOpcode = 3,
            DivideError = 4,
            Watchdog = 5,
            Assert = 6,
            IpcProtocol = 7,
        }
    }

    wire_struct! {
        pub struct FaultReport {
            pub seq: u64,
            pub ts_ns: u64,
            pub pid: u32,
            pub service: String,
            pub kind: FaultKind,
            /// The faulting instruction, and the address it touched (page faults).
            pub rip: u64,
            pub addr: u64,
            /// ≤ 512 bytes.
            pub message: String,
            /// ≤ 16 return addresses, innermost first.
            pub frames: Vec<u64>,
            pub restarted: bool,
        }
    }

    wire_struct! {
        pub struct Faults { pub reports: Vec<FaultReport>, pub next_cursor: u64 }
    }

    /// The boot's stages (PRD § 3), by id.
    pub const STAGES: [&str; 15] =
        ["F0", "F1", "B0", "B1", "B2", "B3", "K0", "K1", "K2", "K3", "C0", "S1", "S2", "S3", "S4"];

    wire_struct! {
        /// Each stage's id (PRD § 3) and its time in ns since B0.
        pub struct BootTimeline { pub stages: Vec<(u8, u64)> }
    }

    wire_struct! {
        /// `watch`'s events.
        pub struct ServiceEvent { pub name: String, pub state: ServiceState }
    }

    wire_struct! {
        /// `stamp`: the stage (13 for S3, 14 for S4) and the TSC when it
        /// happened. Only stage may stamp S3 and only desktop S4, once each.
        pub struct Stamp { pub stage: u8, pub tsc: u64 }
    }
}

/// The logger.
pub mod logger {
    use crate::wire_struct;
    use alloc::string::String;
    use alloc::vec::Vec;

    /// One-way (an event): a client never waits on the log.
    pub const WRITE: u32 = 1;
    pub const READ: u32 = 2;
    pub const SUBSCRIBE: u32 = 3;

    /// Levels: 0 Trace, 1 Debug, 2 Info, 3 Warn, 4 Error.
    pub const TRACE: u8 = 0;
    pub const DEBUG: u8 = 1;
    pub const INFO: u8 = 2;
    pub const WARN: u8 = 3;
    pub const ERROR: u8 = 4;

    wire_struct! {
        pub struct Write { pub level: u8, pub msg: String }
    }

    wire_struct! {
        pub struct Filter { pub svc: Option<String>, pub min_level: Option<u8> }
    }

    wire_struct! {
        pub struct Read { pub cursor: u64, pub limit: u16, pub filter: Filter }
    }

    wire_struct! {
        pub struct LogRecord { pub seq: u64, pub ts_ns: u64, pub level: u8, pub svc: String, pub msg: String }
    }

    wire_struct! {
        /// `next_cursor` 0 when nothing is left; `newest` the newest record's
        /// number (0: none yet).
        pub struct Records { pub records: Vec<LogRecord>, pub next_cursor: u64, pub newest: u64 }
    }
}

/// devmgr (TASK-041): the hardware it found, and each driver's resources.
pub mod devmgr {
    use crate::{HandleRef, wire_struct};
    use alloc::string::String;
    use alloc::vec::Vec;

    pub const LIST: u32 = 1;
    pub const TABLES: u32 = 2;
    pub const FIRMWARE_TIMES: u32 = 3;
    /// A driver asks for its device's resources (devmgr knows it by the
    /// name conductor gave its connection).
    pub const RESOURCES: u32 = 4;
    /// What else devmgr found, and which driver took each device
    /// (TASK-069's probe, TASK-075's Monitor → Devices).
    pub const SUMMARY: u32 = 5;

    wire_struct! {
        /// A device a driver took: `device` names it ("PS/2 controller",
        /// "COM2", "framebuffer", or a PCI address "00:02.0"), `driver` the
        /// service that asked for it.
        pub struct Bound { pub device: String, pub driver: String }
    }

    wire_struct! {
        /// The FADT's 8042 flag (no FADT: assumed present), how many CPUs the
        /// MADT lists, whether there's an HPET, and every device bound so far.
        pub struct Summary { pub has_8042: bool, pub madt_cpus: u32, pub hpet: bool, pub bound: Vec<Bound> }
    }

    wire_struct! {
        pub struct PciDevice {
            pub segment: u16,
            pub bus: u8,
            pub device: u8,
            pub function: u8,
            pub vendor: u16,
            pub id: u16,
            pub class: u8,
            pub subclass: u8,
            pub prog_if: u8,
        }
    }

    wire_struct! {
        pub struct Devices { pub pci: Vec<PciDevice> }
    }

    wire_struct! {
        /// Each ACPI table: its signature, physical address and length.
        pub struct Tables { pub tables: Vec<(String, (u64, u32))> }
    }

    wire_struct! {
        /// FPDT's boot record, in ns since reset (0 when the firmware has none).
        /// And the firmware's vendor (BootInfo v3; empty from an older
        /// bootloader): Monitor says "Firmware (HP)".
        pub struct FirmwareTimes { pub reset_end: u64, pub os_loader_start: u64, pub vendor: String }
    }

    wire_struct! {
        /// A driver's resources, by name (handles in the message), and numbers
        /// that go with them (a framebuffer's size and format, an IRQ's GSI...).
        pub struct Resources { pub handles: Vec<(String, HandleRef)>, pub values: Vec<(String, u64)> }
    }
}

/// The framebuffer driver (TASK-042).
pub mod display {
    use crate::{HandleRef, wire_struct};

    pub const INFO: u32 = 1;
    pub const FILL: u32 = 2;

    wire_struct! {
        /// The framebuffer: its VMO (handle 0, write-combining), size and
        /// layout; `format` 0 is BGRX (blue first), 1 RGBX.
        pub struct Info {
            pub vmo: HandleRef,
            pub width: u32,
            pub height: u32,
            pub stride: u32,
            pub format: u32,
            pub edid_size: u32,
        }
    }

    wire_struct! {
        /// Fills the whole screen with one colour, `0xRRGGBB`.
        pub struct Fill { pub rgb: u32 }
    }
}

/// Keyboards and mice (TASK-043): a subscriber gets every event.
pub mod input {
    use crate::wire_struct;

    pub const SUBSCRIBE: u32 = 1;
    /// Events.
    pub const KEY: u32 = 2;
    pub const MOUSE: u32 = 3;

    wire_struct! {
        /// A key went down or up: its Freally key code (the HID usage), the
        /// character the US layout gives it (0 for none), and the modifiers
        /// held (bit 0 shift, 1 ctrl, 2 alt, 3 meta, 4 caps lock).
        pub struct Key { pub usage: u16, pub down: bool, pub ch: u32, pub modifiers: u8 }
    }

    wire_struct! {
        /// Relative motion and the wheel in HID's directions (right, down and
        /// scrolling up positive), and the buttons held (bit 0 left, 1 right,
        /// 2 middle).
        pub struct Mouse { pub dx: i16, pub dy: i16, pub wheel: i8, pub buttons: u8 }
    }
}

/// Serial ports (TASK-044, dev and e2e): `open` gives one client a port's channel.
pub mod serial {
    use crate::{HandleRef, wire_struct};
    use alloc::vec::Vec;

    pub const OPEN: u32 = 1;
    /// On a port's channel, driver to client (events): bytes that arrived.
    pub const DATA: u32 = 2;
    /// On a port's channel, client to driver (one-way): bytes to send.
    pub const WRITE: u32 = 3;

    wire_struct! {
        /// 2 for COM2 (the Brain's), 3 for COM3 (cdpd's).
        pub struct Open { pub port: u8 }
    }

    wire_struct! {
        pub struct Opened { pub channel: HandleRef }
    }

    wire_struct! {
        pub struct Bytes { pub data: Vec<u8> }
    }
}

/// Stage (TASK-057, PRD § 4): windows, their surfaces, and input. A window's
/// surface is a VMO of premultiplied BGRA pixels (`w × h × 4` bytes, rows
/// tightly packed) that the client draws into and `present`s.
pub mod stage {
    use crate::{HandleRef, wire_enum, wire_struct};
    use alloc::string::String;
    use alloc::vec::Vec;

    pub const CREATE_WINDOW: u32 = 1;
    pub const PRESENT: u32 = 2;
    pub const RESIZE_SURFACE: u32 = 3;
    pub const SET_TITLE: u32 = 4;
    pub const CLOSE_WINDOW: u32 = 5;
    pub const FOCUS_WINDOW: u32 = 6;
    pub const SET_CURSOR: u32 = 7;
    pub const PUBLISH_AX_TREE: u32 = 8;
    pub const SCREENSHOT: u32 = 9;
    pub const INJECT_INPUT: u32 = 10;
    pub const SET_THEME: u32 = 11;
    pub const SET_SCALE: u32 = 12;
    /// The desktop's: the global shortcuts that are its to act on (the
    /// launcher, Monitor, Terminal), on a channel of their own.
    pub const SUBSCRIBE_SHORTCUTS: u32 = 13;
    /// The screen's size, the UI scale and the theme.
    pub const SCREEN: u32 = 14;
    /// The desktop only: bring an app's most recently used window forward
    /// (`RaiseApp` → `bool`, whether it had one).
    pub const RAISE_APP: u32 = 15;
    /// `set_saver` (settings; the e2e door): which screensaver, and after how
    /// long idle (ms; 0: never). TASK-077C, FR-091.
    pub const SET_SAVER: u32 = 16;
    /// `set_motion` (settings; the e2e door): Reduce Motion on or off (the
    /// cursor's trail, the savers).
    pub const SET_MOTION: u32 = 17;
    /// `set_wallpaper` (`SetWallpaper`; settings and accounts): one of
    /// the Freally wallpapers, by name (`studio` …) (TASK-104).
    pub const SET_WALLPAPER: u32 = 18;
    /// `()` → `Look`: what the screen shows now (anyone; TASK-104).
    pub const LOOK: u32 = 19;
    /// `set_clock` (`SetClock`; settings and accounts): the Rack's clock,
    /// 12- or 24-hour; the desktop hears `EV_CLOCK`.
    pub const SET_CLOCK: u32 = 20;
    /// `Carry` → `()`: while the pointer is held on the caller's window,
    /// stage carries `data` (a path, for Files); let go over another window,
    /// that one hears `EV_DROP` (TASK-105: drag between windows).
    pub const CARRY: u32 = 21;
    /// `Clip` → `()`: text on the clipboard (TASK-112: Copy). Only from the
    /// window that has the focus: copying is the person's own act.
    pub const SET_CLIPBOARD: u32 = 22;
    /// `ById` → `ClipText`: what's on the clipboard (Paste); only to the
    /// focused window, so nothing in the background reads it.
    pub const CLIPBOARD: u32 = 23;
    /// The clipboard's most text.
    pub const CLIP_MAX: usize = 64 * 1024;
    /// The e2e door's read-backs (e2e images only; conductor's door only).
    pub const LIST_WINDOWS: u32 = 100;
    pub const POINTER: u32 = 101;
    /// e2e images only, conductor and cdpd only: a window's published
    /// accessibility tree (`ById` → `AxBytes`).
    pub const AX_TREE: u32 = 102;

    /// Events on a window's channel, stage to client.
    pub const EV_KEY: u32 = 1;
    pub const EV_POINTER: u32 = 2;
    pub const EV_FOCUS: u32 = 3;
    pub const EV_RESIZE: u32 = 4;
    pub const EV_THEME: u32 = 5;
    pub const EV_SCALE: u32 = 6;
    pub const EV_CLOSE_REQUESTED: u32 = 7;
    /// On the shortcuts channel.
    pub const EV_SHORTCUT: u32 = 8;
    /// To the desktop (on its shortcuts channel): a screensaver starts or
    /// ends (`SaverEvent`).
    pub const EV_SAVER: u32 = 9;
    /// To the desktop: the clock's format changed (`SetClock`).
    pub const EV_CLOCK: u32 = 10;
    /// What another window carried, let go over this one (`DropEvent`).
    pub const EV_DROP: u32 = 11;

    wire_enum! {
        pub enum Kind {
            /// Decorated, placed by stage, in Alt+Tab.
            Normal = 0,
            /// Undecorated, below every normal window, placed by its client
            /// (the desktop's wallpaper and Rack).
            Panel = 1,
            /// Undecorated, above every normal window, placed by its client.
            Popup = 2,
            /// The whole screen, undecorated.
            Fullscreen = 3,
            /// Above everything, placed by its client, never focused, and the
            /// pointer passes through it.
            Tooltip = 4,
            /// The whole screen, above everything else, never focused: a
            /// screensaver (the desktop's). While one is up, any input ends it
            /// and does nothing else (TASK-077C, FR-091).
            Saver = 5,
            /// The whole screen, undecorated, over the desktop's panels (the
            /// Rack) and under popups: Welcome and Login (TASK-102), which
            /// nothing of the desktop may show through or take a click from.
            Lock = 6,
        }
    }

    wire_struct! {
        /// `x`, `y` place a Panel or Popup; stage places the others.
        pub struct CreateWindow {
            pub title: String,
            pub w: u32,
            pub h: u32,
            pub min_w: u32,
            pub min_h: u32,
            pub kind: Kind,
            pub x: i32,
            pub y: i32,
        }
    }

    wire_struct! {
        /// The surface (handle 0) and the window's event channel (handle 1).
        pub struct Created { pub window_id: u32, pub surface: HandleRef, pub events: HandleRef }
    }

    wire_struct! {
        pub struct Rect { pub x: i32, pub y: i32, pub w: u32, pub h: u32 }
    }

    wire_struct! {
        /// The parts of the surface that changed, in its own coordinates.
        pub struct Present { pub window_id: u32, pub damage: Vec<Rect> }
    }

    wire_struct! {
        pub struct SetTitle { pub window_id: u32, pub title: String }
    }

    wire_struct! {
        /// `close_window` and `focus_window`.
        pub struct ById { pub window_id: u32 }
    }

    wire_struct! {
        /// `set_cursor`: 0 Arrow, 1 Hand, 2 Text, 3 ResizeEW, 4 ResizeNS,
        /// 5 ResizeNWSE, 6 ResizeNESW, 7 Move, 8 Crosshair, 9 Grab, 10
        /// NotAllowed. Arrow and Hand are the fleet cursor (the ring and dot).
        pub struct SetCursor { pub window_id: u32, pub kind: u8 }
    }

    wire_enum! {
        pub enum Theme {
            Dark = 0,
            Light = 1,
        }
    }

    wire_struct! {
        pub struct SetTheme { pub theme: Theme }
    }

    wire_struct! {
        /// `set_scale`: 100 to 300 %, in steps of 25 (FR-031); 0, the screen's
        /// own, from its EDID (TASK-104). Its event carries the scale in use.
        pub struct SetScale { pub pct: u16 }
    }

    wire_struct! {
        /// `screenshot` (brain and cdpd only): the whole screen, or one
        /// window's frame.
        pub struct Screenshot { pub window_id: Option<u32> }
    }

    wire_struct! {
        /// The picture (handle 0): `w × h` pixels, 0x00RRGGBB, one u32 each,
        /// the cursor in it, as the screen shows.
        pub struct Shot { pub vmo: HandleRef, pub w: u32, pub h: u32 }
    }

    wire_struct! {
        /// `inject_input` (brain in dev images, cdpd in e2e images): a key or
        /// a mouse report, as if drv-i8042 had sent it.
        pub struct Inject { pub key: Option<crate::proto::input::Key>, pub mouse: Option<crate::proto::input::Mouse> }
    }

    wire_struct! {
        /// `publish_ax_tree`: the window's accessibility tree (handle 0), a
        /// VMO holding its length (u64, little-endian) and then the tree
        /// (`crate::proto::ax::Tree`).
        pub struct AxTree { pub window_id: u32, pub tree: HandleRef }
    }

    wire_struct! {
        /// `raise_app`: the service whose window comes forward.
        pub struct RaiseApp { pub name: String }
    }

    wire_struct! {
        /// `ax_tree`'s answer: the tree's bytes as published (empty: none).
        pub struct AxBytes { pub window_id: u32, pub bytes: Vec<u8> }
    }

    wire_struct! {
        /// One window as the e2e door reads it back: its frame on screen (the
        /// title strip included), whether it has focus, its place in the
        /// stacking order (0 at the bottom), and its owner.
        pub struct WindowInfo {
            pub window_id: u32,
            pub title: String,
            pub kind: Kind,
            pub frame: Rect,
            pub focused: bool,
            pub z: u32,
            pub owner: String,
            /// The accessibility tree it last published, in bytes (0: none yet).
            pub ax_bytes: u64,
            /// Its surface on screen (the frame less the title strip).
            pub surface: Rect,
        }
    }

    wire_struct! {
        pub struct Windows { pub windows: Vec<WindowInfo>, pub theme: Theme, pub scale_pct: u16 }
    }

    wire_struct! {
        pub struct PointerAt { pub x: i32, pub y: i32, pub buttons: u8 }
    }

    wire_struct! {
        /// `EV_KEY`: as drv-i8042 gave it.
        pub struct Key { pub usage: u16, pub down: bool, pub ch: u32, pub modifiers: u8 }
    }

    wire_enum! {
        pub enum PointerKind {
            Move = 0,
            Down = 1,
            Up = 2,
            Wheel = 3,
            /// The pointer left the window (or went onto another one above it).
            Leave = 4,
        }
    }

    wire_struct! {
        /// `EV_POINTER`: in the window's surface coordinates; `dy` is the
        /// wheel (up positive) for `Wheel`.
        pub struct Pointer { pub x: i32, pub y: i32, pub buttons: u8, pub kind: PointerKind, pub dx: i32, pub dy: i32 }
    }

    wire_struct! {
        pub struct Focus { pub focused: bool }
    }

    wire_enum! {
        pub enum Shortcut {
            Launcher = 0,
            Monitor = 1,
            Terminal = 2,
            Settings = 3,
            Files = 4,
        }
    }

    wire_struct! {
        pub struct ShortcutEvent { pub shortcut: Shortcut }
    }

    wire_struct! {
        /// `set_saver`: 0 Bounce, 1 the turning block, 2 Swarm, 3 Warp; idle
        /// time in ms (0: never).
        pub struct SetSaver { pub kind: u8, pub idle_ms: u32 }
    }

    wire_struct! {
        pub struct SetMotion { pub reduce: bool }
    }

    wire_struct! {
        pub struct SetWallpaper { pub name: String }
    }

    /// The Freally wallpapers, by `Look::wallpaper`.
    pub const WALLPAPERS: [&str; 4] = ["studio", "circuit", "orbit", "connected"];
    /// `Look::saver` when there's none.
    pub const NO_SAVER: u8 = 255;

    wire_struct! {
        pub struct SetClock { pub h24: bool }
    }

    wire_struct! {
        pub struct Carry { pub window_id: u32, pub data: String }
    }

    wire_struct! {
        pub struct Clip { pub window_id: u32, pub text: String }
    }

    wire_struct! {
        pub struct ClipText { pub text: String }
    }

    wire_struct! {
        /// Where it was let go (surface pixels), what was carried, and the
        /// modifiers held as it was (Ctrl: a copy; Shift: a move).
        pub struct DropEvent { pub x: i32, pub y: i32, pub data: String, pub modifiers: u8 }
    }

    wire_struct! {
        /// A person's look (TASK-104; the account record's bytes 11–13 and
        /// 449–454, PRD § 3). `scale_pct` 0: the screen's own (its EDID's).
        pub struct Look {
            pub dark: bool,
            pub scale_pct: u16,
            /// An index into `WALLPAPERS`.
            pub wallpaper: u8,
            pub reduce_motion: bool,
            pub clock_24h: bool,
            /// 0 Bounce, 1 the turning block, 2 Swarm, 3 Warp; `NO_SAVER`.
            pub saver: u8,
            pub saver_idle_min: u16,
        }
    }

    wire_struct! {
        /// `EV_SAVER`: a screensaver starts (which, and whether Reduce Motion
        /// asks for the still F) or ends.
        pub struct SaverEvent { pub start: bool, pub kind: u8, pub reduce_motion: bool }
    }

    wire_struct! {
        /// `subscribe_shortcuts`' reply: the channel (handle 0) they arrive on.
        pub struct Subscribed { pub events: HandleRef }
    }

    wire_struct! {
        /// `work_h`: the height a window's frame has (the screen less the
        /// Rack), so a window is made to fit it.
        pub struct ScreenInfo { pub w: u32, pub h: u32, pub scale_pct: u16, pub theme: Theme, pub work_h: u32 }
    }
}

/// Freally Terminal's test door (TASK-060, e2e images only, conductor's door
/// only): bytes straight to the screen, keys to the shell, the screen read
/// back.
pub mod terminal {
    use crate::wire_struct;
    use alloc::string::String;
    use alloc::vec::Vec;

    /// The program's output, straight to the screen (a recorded sequence).
    pub const FEED: u32 = 1;
    /// Bytes as if typed (to the shell).
    pub const TYPE: u32 = 2;
    pub const SCREEN: u32 = 3;

    wire_struct! {
        pub struct Bytes { pub data: Vec<u8> }
    }

    wire_struct! {
        /// Each row's text (trailing spaces dropped), the cursor, the
        /// scrollback's length, and the window's frame on screen.
        pub struct Screen {
            pub rows: Vec<String>,
            pub cursor: (u32, u32),
            pub scrollback: u32,
            pub cols: u32,
            pub window: u32,
        }
    }
}

/// Monitor's test door (TASK-062, e2e images only, conductor's door only):
/// what it shows, as text.
pub mod monitor {
    use crate::wire_struct;
    use alloc::string::String;
    use alloc::vec::Vec;

    pub const STATE: u32 = 1;

    wire_struct! {
        /// The tab shown, the Processes sort, the Log filter, the tab's lines
        /// as text, and the window.
        pub struct State { pub tab: String, pub sort: String, pub filter: String, pub lines: Vec<String>, pub window: u32 }
    }
}

/// A window's accessibility tree (TASK-063, PRD § 4): what stage keeps for
/// cdpd, which reads it as the DOM. Nodes in order, each after its parent;
/// bounds in surface pixels.
pub mod ax {
    use crate::wire_struct;
    use alloc::string::String;
    use alloc::vec::Vec;

    /// A root's parent.
    pub const ROOT: u32 = u32::MAX;

    /// Roles, in `fui::a11y::Role`'s order; `NAMES` are the DOM's node names.
    pub const WINDOW: u8 = 0;
    pub const GROUP: u8 = 1;
    pub const TEXT: u8 = 2;
    pub const BUTTON: u8 = 3;
    pub const SWITCH: u8 = 4;
    pub const SLIDER: u8 = 5;
    pub const KNOB: u8 = 6;
    pub const METER: u8 = 7;
    pub const TEXTBOX: u8 = 8;
    pub const LISTBOX: u8 = 9;
    pub const OPTION: u8 = 10;
    pub const TABLE: u8 = 11;
    pub const ROW: u8 = 12;
    pub const CELL: u8 = 13;
    pub const TABLIST: u8 = 14;
    pub const TAB: u8 = 15;
    pub const MENU: u8 = 16;
    pub const MENUITEM: u8 = 17;
    pub const DIALOG: u8 = 18;
    pub const TOOLTIP: u8 = 19;
    pub const IMG: u8 = 20;
    pub const SCROLLAREA: u8 = 21;
    pub const NAMES: [&str; 22] = [
        "WINDOW",
        "GROUP",
        "TEXT",
        "BUTTON",
        "SWITCH",
        "SLIDER",
        "KNOB",
        "METER",
        "TEXTBOX",
        "LISTBOX",
        "OPTION",
        "TABLE",
        "ROW",
        "CELL",
        "TABLIST",
        "TAB",
        "MENU",
        "MENUITEM",
        "DIALOG",
        "TOOLTIP",
        "IMG",
        "SCROLLAREA",
    ];
    /// The roles a person works: each must be worked by some spec (the
    /// inventory).
    pub fn interactive(role: u8) -> bool {
        matches!(role, BUTTON | SWITCH | SLIDER | KNOB | TEXTBOX | OPTION | TAB | MENUITEM)
    }

    /// State bits (`fui::a11y::state`).
    pub const FOCUSED: u32 = 1 << 0;
    pub const PRESSED: u32 = 1 << 1;
    pub const CHECKED: u32 = 1 << 2;
    pub const DISABLED: u32 = 1 << 3;
    pub const EXPANDED: u32 = 1 << 4;
    pub const SELECTED: u32 = 1 << 5;
    pub const HOVERED: u32 = 1 << 6;

    wire_struct! {
        pub struct Node {
            pub parent: u32,
            pub role: u8,
            pub name: String,
            pub state: u32,
            pub value: String,
            pub testid: String,
            pub x: i32,
            pub y: i32,
            pub w: u32,
            pub h: u32,
        }
    }

    wire_struct! {
        pub struct Tree { pub nodes: Vec<Node> }
    }
}

/// cdpd (TASK-063, e2e images only): conductor's test door hands it one CDP
/// method at a time, its params and answer as JSON text.
pub mod cdpd {
    use crate::wire_struct;
    use alloc::string::String;

    pub const CALL: u32 = 1;

    wire_struct! {
        pub struct Call { pub method: String, pub params: String }
    }

    wire_struct! {
        pub struct Answer { pub result: String }
    }
}

/// The block protocol (TASK-100, PRD § 9): drv-nvme to ffs. 4 KiB blocks
/// within a partition named by its GPT name; every write passes the
/// partition interlock first.
pub mod block {
    use crate::wire_struct;
    use alloc::string::String;
    use alloc::vec::Vec;

    /// `()` → `Info`.
    pub const INFO: u32 = 1;
    /// `Read` → `Data`.
    pub const READ: u32 = 2;
    /// `Write` → `()`.
    pub const WRITE: u32 = 3;
    /// `()` → `()`: everything written is durable.
    pub const FLUSH: u32 = 4;
    /// The most blocks one read or write carries (a message holds 64 KiB).
    pub const MAX_BLOCKS: u32 = 15;

    wire_struct! {
        /// A partition: its GPT name, its size in 4 KiB blocks, and whether
        /// it's Freally OS's (writable through the interlock).
        pub struct Partition { pub name: String, pub blocks: u64, pub freally: bool }
    }
    wire_struct! {
        pub struct Info { pub model: String, pub partitions: Vec<Partition> }
    }
    wire_struct! {
        pub struct Read { pub partition: String, pub block: u64, pub count: u32 }
    }
    wire_struct! {
        pub struct Data { pub bytes: Vec<u8> }
    }
    wire_struct! {
        pub struct Write { pub partition: String, pub block: u64, pub bytes: Vec<u8> }
    }
}

/// FreallyFS's protocol (TASK-100): ffs to vfs (and accounts, TASK-101):
/// a subvolume, an inode, a name.
pub mod ffs {
    use crate::wire_struct;
    use alloc::string::String;
    use alloc::vec::Vec;

    /// `Name` → `Entry`.
    pub const LOOKUP: u32 = 1;
    /// `Ino` → `Stat`.
    pub const STAT: u32 = 2;
    /// `Ino` → `Entries`.
    pub const READDIR: u32 = 3;
    /// `Create` → `Entry`.
    pub const CREATE: u32 = 4;
    /// `Read` → `Data`.
    pub const READ: u32 = 5;
    /// `Write` → `()`.
    pub const WRITE: u32 = 6;
    /// `Truncate` → `()`.
    pub const TRUNCATE: u32 = 7;
    /// `Name` → `()`.
    pub const UNLINK: u32 = 8;
    /// `Rename` → `()`.
    pub const RENAME: u32 = 9;
    /// `()` → `()`: commit.
    pub const FSYNC: u32 = 10;
    /// `Snapshot` → `Subvol`.
    pub const SNAPSHOT: u32 = 11;
    /// `Named` → `Subvol`: the subvolume called this.
    pub const FIND: u32 = 12;
    /// `Home` → `Subvol`: a new encrypted home (accounts; e2e: the door).
    pub const CREATE_HOME: u32 = 13;
    /// `Unlock` → `()`.
    pub const UNLOCK: u32 = 14;
    /// `Subvol` → `()`: commit, then forget the key.
    pub const LOCK: u32 = 15;
    /// `()` → `()`: a commit with the clean flag (conductor, just before
    /// Shut down or Restart: the next mount needs no ffsck).
    pub const UNMOUNT: u32 = 16;
    /// `Subvol` → `()` (accounts, an account removed; TASK-104): the home
    /// renamed `retired:<sv>` and kept. Its key went with the account's
    /// record, so nothing can read it again; its blocks are freed once
    /// FreallyFS tracks blocks by subvolume (a sealed tree can't be walked).
    pub const RETIRE: u32 = 17;
    /// `CloneFile` → `Entry`: a reflink in one subvolume, every block shared
    /// (vfs's copy engine; TASK-105A).
    pub const CLONE: u32 = 18;
    /// `Times` → `()`: a file's modified time set (a copy keeps its source's).
    pub const SET_MODIFIED: u32 = 19;

    /// Inode kinds as the protocol carries them.
    pub const FILE: u8 = 1;
    pub const DIR: u8 = 2;

    wire_struct! {
        /// An inode of a subvolume.
        pub struct Ino { pub sv: u64, pub ino: u64 }
    }
    wire_struct! {
        /// A name in a directory.
        pub struct Name { pub sv: u64, pub dir: u64, pub name: String }
    }
    wire_struct! {
        pub struct Entry { pub ino: u64, pub kind: u8 }
    }
    wire_struct! {
        pub struct Entries { pub entries: Vec<(String, Entry)> }
    }
    wire_struct! {
        pub struct Stat { pub kind: u8, pub size: u64, pub nlink: u32, pub modified_ns: u64, pub created_ns: u64 }
    }
    wire_struct! {
        pub struct Create { pub sv: u64, pub dir: u64, pub name: String, pub kind: u8 }
    }
    wire_struct! {
        pub struct Read { pub sv: u64, pub ino: u64, pub offset: u64, pub len: u32 }
    }
    wire_struct! {
        pub struct Data { pub bytes: Vec<u8> }
    }
    wire_struct! {
        pub struct Write { pub sv: u64, pub ino: u64, pub offset: u64, pub bytes: Vec<u8> }
    }
    wire_struct! {
        pub struct Truncate { pub sv: u64, pub ino: u64, pub size: u64 }
    }
    wire_struct! {
        pub struct Rename { pub sv: u64, pub dir: u64, pub name: String, pub to_dir: u64, pub to_name: String }
    }
    wire_struct! {
        pub struct Snapshot { pub sv: u64, pub name: String }
    }
    wire_struct! {
        pub struct Named { pub name: String }
    }
    wire_struct! {
        pub struct Subvol { pub sv: u64 }
    }
    wire_struct! {
        /// A home: its subvolume's name (`home:<user>`), its owner's id, its key.
        pub struct Home { pub name: String, pub owner: [u8; 16], pub key: [u8; 32] }
    }
    wire_struct! {
        pub struct Unlock { pub sv: u64, pub key: [u8; 32] }
    }
    wire_struct! {
        pub struct CloneFile { pub sv: u64, pub src: u64, pub dir: u64, pub name: String }
    }
    wire_struct! {
        pub struct Times { pub sv: u64, pub ino: u64, pub modified_ns: u64 }
    }
}

/// The file protocol (FR-043, TASK-100): every process to vfs, by path.
/// `/system` is read-only for everyone; `/home/<user>` belongs to the
/// session's account alone.
pub mod vfs {
    use crate::wire_struct;
    use alloc::string::String;
    use alloc::vec::Vec;

    /// `Open` → `Opened`.
    pub const OPEN: u32 = 1;
    /// `ReadAt` → `Data`: from the handle's place, which moves on.
    pub const READ: u32 = 2;
    /// `WriteAt` → `()`: at the handle's place, which moves on.
    pub const WRITE: u32 = 3;
    /// `Seek` → `()`.
    pub const SEEK: u32 = 4;
    /// `Handle` → `()`.
    pub const CLOSE: u32 = 5;
    /// `Path` → `Stat`.
    pub const STAT: u32 = 6;
    /// `Path` → `Entries`.
    pub const READDIR: u32 = 7;
    /// `Path` → `()`.
    pub const MKDIR: u32 = 8;
    /// `Rename` → `()`.
    pub const RENAME: u32 = 9;
    /// `Path` → `()`.
    pub const UNLINK: u32 = 10;
    /// `()` → `()`: everything written is durable.
    pub const FSYNC: u32 = 11;
    /// `Snapshot` → `()`: a read-only snapshot of the session's home.
    pub const SNAPSHOT: u32 = 12;
    /// `()` → `Who`: the session's account (empty: none).
    pub const WHOAMI: u32 = 13;
    /// `Session` → `()`: accounts (and, in e2e images, conductor's door) sets the session.
    pub const SET_SESSION: u32 = 14;
    /// `CopyStart` → `CopyJob`: a copy or move job (TASK-105A, FR-085),
    /// checked and weighed; its caller steps it.
    pub const COPY_START: u32 = 15;
    /// `CopyRef` → `CopyState`: up to 4 MiB more done, and where it stands.
    pub const COPY_STEP: u32 = 16;
    /// `CopyDecide` → `()`: a conflict answered (Replace, Skip, Keep both;
    /// for this one or all), or a failure (Try again, Skip).
    pub const COPY_DECIDE: u32 = 17;
    /// `CopyRef` → `CopyConflicts`: every conflict ahead (*Let me decide for
    /// each file*).
    pub const COPY_CONFLICTS: u32 = 18;
    /// `CopyEach` → `()`: a choice per file.
    pub const COPY_EACH: u32 = 19;
    /// `CopyRef` → `()`: stopped; a half-copied file is removed.
    pub const COPY_CANCEL: u32 = 20;

    /// `CopyState::state`.
    pub const COPY_RUNNING: u8 = 0;
    pub const COPY_CONFLICT: u8 = 1;
    pub const COPY_FAILED: u8 = 2;
    pub const COPY_DONE: u8 = 3;
    pub const COPY_CANCELLED: u8 = 4;
    /// `CopyDecide::choice`.
    pub const CHOOSE_REPLACE: u8 = 0;
    pub const CHOOSE_SKIP: u8 = 1;
    pub const CHOOSE_KEEP_BOTH: u8 = 2;
    pub const CHOOSE_TRY_AGAIN: u8 = 3;

    wire_struct! {
        /// Open flags: `write`, `create` (if missing), `truncate`.
        pub struct Open { pub path: String, pub write: bool, pub create: bool, pub truncate: bool }
    }
    wire_struct! {
        pub struct Opened { pub handle: u32, pub size: u64 }
    }
    wire_struct! {
        pub struct Handle { pub handle: u32 }
    }
    wire_struct! {
        pub struct ReadAt { pub handle: u32, pub len: u32 }
    }
    wire_struct! {
        pub struct Data { pub bytes: Vec<u8> }
    }
    wire_struct! {
        pub struct WriteAt { pub handle: u32, pub bytes: Vec<u8> }
    }
    wire_struct! {
        pub struct Seek { pub handle: u32, pub to: u64 }
    }
    wire_struct! {
        pub struct Path { pub path: String }
    }
    wire_struct! {
        /// `dir`; the size in bytes (a directory: its entries); times (Unix ns).
        pub struct Stat { pub dir: bool, pub size: u64, pub modified_ns: u64, pub created_ns: u64 }
    }
    wire_struct! {
        /// Each name and whether it's a directory.
        pub struct Entries { pub entries: Vec<(String, bool)> }
    }
    wire_struct! {
        pub struct Rename { pub from: String, pub to: String }
    }
    wire_struct! {
        pub struct Snapshot { pub name: String }
    }
    wire_struct! {
        pub struct Who { pub user: String }
    }
    wire_struct! {
        /// The session's user (empty: none) and their home's subvolume.
        pub struct Session { pub user: String, pub sv: u64 }
    }
    wire_struct! {
        /// `moving`: a move (the sources go); `verify`: read each copy back.
        pub struct CopyStart { pub sources: Vec<String>, pub dest: String, pub moving: bool, pub verify: bool }
    }
    wire_struct! {
        pub struct CopyJob { pub job: u32, pub total_items: u64, pub total_bytes: u64 }
    }
    wire_struct! {
        pub struct CopyRef { pub job: u32 }
    }
    wire_struct! {
        /// A conflict: the file being copied and the one in its way.
        pub struct CopyConflict {
            pub src: String,
            pub dst: String,
            pub src_size: u64,
            pub src_modified_ns: u64,
            pub dst_size: u64,
            pub dst_modified_ns: u64,
        }
    }
    wire_struct! {
        /// `conflict`: when `state` is `COPY_CONFLICT`; `words`: what went
        /// wrong (`COPY_FAILED`).
        pub struct CopyState {
            pub state: u8,
            pub done_items: u64,
            pub total_items: u64,
            pub done_bytes: u64,
            pub total_bytes: u64,
            pub current: String,
            pub conflict: CopyConflict,
            pub words: String,
        }
    }
    wire_struct! {
        pub struct CopyDecide { pub job: u32, pub choice: u8, pub all: bool }
    }
    wire_struct! {
        pub struct CopyConflicts { pub items: Vec<CopyConflict> }
    }
    wire_struct! {
        /// Each source path and its choice.
        pub struct CopyEach { pub job: u32, pub choices: Vec<(String, u8)> }
    }
}

/// The accounts protocol (TASK-101, PRD § 10): the welcome and login
/// screens, Settings → Accounts, and (in e2e images) conductor's test door.
pub mod accounts {
    use crate::wire_struct;
    use alloc::string::String;
    use alloc::vec::Vec;

    /// `()` → `Users`: who can log in (the login screen's list).
    pub const LIST: u32 = 1;
    /// `Create` → `()`: the first account (the Owner) from the welcome
    /// screen; any later one needs the Owner logged in and their password.
    pub const CREATE: u32 = 2;
    /// `Login` → `()`. A wrong password: `AccessDenied`, "That password
    /// didn't match."; from the third in a row, `Busy` until a second more
    /// has passed for each.
    pub const LOGIN: u32 = 3;
    /// `()` → `()`: the home locked, its key zeroed, the session ended.
    pub const LOGOUT: u32 = 4;
    /// `ChangePassword` → `()`: the session's own (the key re-wrapped, the
    /// files untouched).
    pub const CHANGE_PASSWORD: u32 = 5;
    /// `()` → `Who`: the session's account (empty: none).
    pub const SESSION: u32 = 6;
    /// `Export` → `Exported`: the session's account and home, as a
    /// `.facct` sealed with its password (re-typed), into
    /// `/home/<user>/Exports/` (TASK-103; a removable disk first, once
    /// there's a driver for one).
    pub const EXPORT: u32 = 7;
    /// `()` → `Bundles`: the `.facct` files that can be imported:
    /// `system:<name>` in `/system/imports/`, and (logged in)
    /// `home:<name>` in the session's `Exports`.
    pub const BUNDLES: u32 = 8;
    /// `Open` → `Preview`: what a bundle holds, the whole file checked.
    pub const PREVIEW: u32 = 9;
    /// `Import` → `User`: a bundle made into an account (the first is the
    /// Owner; any other needs the Owner logged in and its password).
    pub const IMPORT: u32 = 10;
    /// `stage::Look` → `()`: Settings saves the session account's look
    /// (TASK-104); accounts applies it to stage at once, and at every login.
    pub const SET_LOOK: u32 = 11;
    /// `Remove` → `()`: the Owner removes another account, typing its
    /// username again; its home goes with it.
    pub const REMOVE: u32 = 12;

    wire_struct! {
        /// One account as the login screen shows it.
        pub struct User { pub username: String, pub display_name: String, pub owner: bool }
    }
    wire_struct! {
        pub struct Users { pub users: Vec<User> }
    }
    wire_struct! {
        pub struct Create { pub username: String, pub display_name: String, pub password: String, pub owner_password: String }
    }
    wire_struct! {
        pub struct Login { pub username: String, pub password: String }
    }
    wire_struct! {
        pub struct ChangePassword { pub old: String, pub new: String }
    }
    wire_struct! {
        pub struct Who { pub user: User }
    }
    wire_struct! {
        pub struct Export { pub password: String }
    }
    wire_struct! {
        /// Where it went (a path as vfs names it), and its size.
        pub struct Exported { pub path: String, pub bytes: u64 }
    }
    wire_struct! {
        pub struct Bundles { pub names: Vec<String> }
    }
    wire_struct! {
        pub struct Open { pub name: String, pub password: String }
    }
    wire_struct! {
        pub struct Preview {
            pub username: String,
            pub display_name: String,
            pub files: u64,
            pub bytes: u64,
            pub exported_unix: u64,
        }
    }
    wire_struct! {
        /// `rename`: a username instead of the bundle's (empty: its own).
        pub struct Import { pub name: String, pub password: String, pub rename: String, pub owner_password: String }
    }
    wire_struct! {
        /// `confirm`: the username typed again.
        pub struct Remove { pub username: String, pub confirm: String, pub owner_password: String }
    }
}

/// The inventory's ordinals (TASK-063, FR-040, e2e images only): every
/// process answers `ASK` itself (fstd, never passed to the service) with how
/// often each request ordinal reached it. `SERVICES` lists what each
/// service serves; a full run must touch every one, or accept it.
pub mod inventory {
    use crate::wire_struct;
    use alloc::vec::Vec;

    pub const ASK: u32 = 0xFFFF_FF00;

    wire_struct! {
        /// Each request ordinal and how many times it came.
        pub struct Served { pub ordinals: Vec<(u32, u32)> }
    }

    use super::{accounts as acc, block as blk, ffs as fs, vfs as vf};
    use super::{
        cdpd as cd, conductor as c, devmgr as d, display as disp, input as inp, logger as l, monitor as mon,
        serial as s, stage as st, terminal as term,
    };

    pub const SERVICES: &[(&str, &[(u32, &str)])] = &[
        (
            "conductor",
            &[
                (c::CONNECT, "connect"),
                (c::REGISTER, "register"),
                (c::LIST_SERVICES, "list_services"),
                (c::FAULTS, "faults"),
                (c::BOOT_TIMELINE, "boot_timeline"),
                (c::STAMP, "stamp"),
                (c::MANIFEST, "manifest"),
                (c::DATA, "data"),
                (c::POWER, "power"),
            ],
        ),
        ("logger", &[(l::WRITE, "write"), (l::READ, "read")]),
        (
            "devmgr",
            &[
                (d::LIST, "list"),
                (d::TABLES, "tables"),
                (d::FIRMWARE_TIMES, "firmware_times"),
                (d::RESOURCES, "resources"),
                (d::SUMMARY, "summary"),
            ],
        ),
        ("drv-gop-fb", &[(disp::INFO, "info"), (disp::FILL, "fill")]),
        ("drv-i8042", &[(inp::SUBSCRIBE, "subscribe")]),
        ("drv-uart16550", &[(s::WRITE, "write")]),
        (
            "stage",
            &[
                (st::CREATE_WINDOW, "create_window"),
                (st::PRESENT, "present"),
                (st::SET_TITLE, "set_title"),
                (st::CLOSE_WINDOW, "close_window"),
                (st::FOCUS_WINDOW, "focus_window"),
                (st::SET_CURSOR, "set_cursor"),
                (st::PUBLISH_AX_TREE, "publish_ax_tree"),
                (st::SCREENSHOT, "screenshot"),
                (st::INJECT_INPUT, "inject_input"),
                (st::SET_THEME, "set_theme"),
                (st::SET_SCALE, "set_scale"),
                (st::SUBSCRIBE_SHORTCUTS, "subscribe_shortcuts"),
                (st::SCREEN, "screen"),
                (st::RAISE_APP, "raise_app"),
                (st::SET_SAVER, "set_saver"),
                (st::SET_MOTION, "set_motion"),
                (st::SET_WALLPAPER, "set_wallpaper"),
                (st::LOOK, "look"),
                (st::SET_CLOCK, "set_clock"),
                (st::CARRY, "carry"),
                (st::SET_CLIPBOARD, "set_clipboard"),
                (st::CLIPBOARD, "clipboard"),
                (st::LIST_WINDOWS, "list_windows"),
                (st::POINTER, "pointer"),
                (st::AX_TREE, "ax_tree"),
            ],
        ),
        ("monitor", &[(mon::STATE, "state")]),
        ("terminal", &[(term::FEED, "feed"), (term::TYPE, "type"), (term::SCREEN, "screen")]),
        ("cdpd", &[(cd::CALL, "call")]),
        ("drv-nvme", &[(blk::INFO, "info"), (blk::READ, "read"), (blk::WRITE, "write"), (blk::FLUSH, "flush")]),
        (
            "ffs",
            &[
                (fs::LOOKUP, "lookup"),
                (fs::STAT, "stat"),
                (fs::READDIR, "readdir"),
                (fs::CREATE, "create"),
                (fs::READ, "read"),
                (fs::WRITE, "write"),
                (fs::TRUNCATE, "truncate"),
                (fs::UNLINK, "unlink"),
                (fs::RENAME, "rename"),
                (fs::FSYNC, "fsync"),
                (fs::SNAPSHOT, "snapshot"),
                (fs::FIND, "find"),
                (fs::CREATE_HOME, "create_home"),
                (fs::UNLOCK, "unlock"),
                (fs::LOCK, "lock"),
                (fs::UNMOUNT, "unmount"),
                (fs::RETIRE, "retire"),
                (fs::CLONE, "clone"),
                (fs::SET_MODIFIED, "set_modified"),
            ],
        ),
        (
            "vfs",
            &[
                (vf::OPEN, "open"),
                (vf::READ, "read"),
                (vf::WRITE, "write"),
                (vf::SEEK, "seek"),
                (vf::CLOSE, "close"),
                (vf::STAT, "stat"),
                (vf::READDIR, "readdir"),
                (vf::MKDIR, "mkdir"),
                (vf::RENAME, "rename"),
                (vf::UNLINK, "unlink"),
                (vf::FSYNC, "fsync"),
                (vf::SNAPSHOT, "snapshot"),
                (vf::WHOAMI, "whoami"),
                (vf::SET_SESSION, "set_session"),
                (vf::COPY_START, "copy_start"),
                (vf::COPY_STEP, "copy_step"),
                (vf::COPY_DECIDE, "copy_decide"),
                (vf::COPY_CONFLICTS, "copy_conflicts"),
                (vf::COPY_EACH, "copy_each"),
                (vf::COPY_CANCEL, "copy_cancel"),
            ],
        ),
        (
            "accounts",
            &[
                (acc::LIST, "list"),
                (acc::CREATE, "create"),
                (acc::LOGIN, "login"),
                (acc::LOGOUT, "logout"),
                (acc::CHANGE_PASSWORD, "change_password"),
                (acc::SESSION, "session"),
                (acc::EXPORT, "export"),
                (acc::BUNDLES, "bundles"),
                (acc::PREVIEW, "preview"),
                (acc::IMPORT, "import"),
                (acc::SET_LOOK, "set_look"),
                (acc::REMOVE, "remove"),
            ],
        ),
    ];
}
