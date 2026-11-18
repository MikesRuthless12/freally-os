// SPDX-License-Identifier: Apache-2.0
//! The service protocols (PRD § 4): ordinals and message bodies.

/// Conductor, on every process's well-known handle 1 (the directory channel).
pub mod conductor {
    use crate::{HandleRef, wire_enum, wire_struct};
    use alloc::string::String;
    use alloc::vec::Vec;

    pub const CONNECT: u32 = 1;
    pub const REGISTER: u32 = 2;
    pub const LIST_SERVICES: u32 = 3;
    pub const RESTART: u32 = 4;
    pub const SWAP: u32 = 5;
    pub const ROLLBACK: u32 = 6;
    pub const FAULTS: u32 = 7;
    pub const BOOT_TIMELINE: u32 = 8;
    pub const WATCH: u32 = 9;

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
