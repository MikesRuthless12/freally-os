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
        }
    }

    wire_struct! {
        pub struct Services { pub services: Vec<ServiceInfo> }
    }

    wire_struct! {
        /// `swap` (brain only, dev flavour only): the binary is handle 0.
        pub struct Swap { pub name: String, pub binary: HandleRef, pub signature: [u8; 64] }
    }

    wire_struct! {
        pub struct Swapped { pub old_sha256: [u8; 32], pub new_sha256: [u8; 32], pub ms: u32 }
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

    wire_struct! {
        /// Each stage's id (PRD § 3) and its time in ns since B0.
        pub struct BootTimeline { pub stages: Vec<(u8, u64)> }
    }

    wire_struct! {
        /// `watch`'s events.
        pub struct ServiceEvent { pub name: String, pub state: ServiceState }
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
        pub struct Records { pub records: Vec<LogRecord>, pub next_cursor: u64 }
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
        pub struct FirmwareTimes { pub reset_end: u64, pub os_loader_start: u64 }
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
