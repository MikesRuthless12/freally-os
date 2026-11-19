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
        /// `set_scale`: 100 to 300 %, in steps of 25 (FR-031).
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
        }
    }

    wire_struct! {
        pub struct ShortcutEvent { pub shortcut: Shortcut }
    }

    wire_struct! {
        /// `subscribe_shortcuts`' reply: the channel (handle 0) they arrive on.
        pub struct Subscribed { pub events: HandleRef }
    }

    wire_struct! {
        pub struct ScreenInfo { pub w: u32, pub h: u32, pub scale_pct: u16, pub theme: Theme }
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
                (st::LIST_WINDOWS, "list_windows"),
                (st::POINTER, "pointer"),
                (st::AX_TREE, "ax_tree"),
            ],
        ),
        ("monitor", &[(mon::STATE, "state")]),
        ("terminal", &[(term::FEED, "feed"), (term::TYPE, "type"), (term::SCREEN, "screen")]),
        ("cdpd", &[(cd::CALL, "call")]),
    ];
}
