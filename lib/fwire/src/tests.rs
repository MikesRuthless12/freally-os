// SPDX-License-Identifier: Apache-2.0
//! Every message type round-trips; every malformed message is an error, never a panic.

extern crate std;

use super::*;
use crate::proto::{conductor as c, logger as l};
use alloc::string::ToString;
use alloc::vec;
use core::fmt::Debug;

/// Encodes `v` as a request, a reply and an event; each parses back to `v`.
fn round_trip<T: Wire + PartialEq + Debug>(ordinal: u32, v: T) {
    for (msg, flags) in [
        (request(7, ordinal, &v).unwrap(), 0),
        (reply(7, ordinal, &v).unwrap(), flags::REPLY),
        (event(ordinal, &v).unwrap(), flags::EVENT),
    ] {
        let (h, payload) = parse(&msg).unwrap();
        assert_eq!(h.ordinal, ordinal);
        assert_eq!(h.flags, flags);
        assert_eq!(from_payload::<T>(payload).unwrap(), v);
        // Every prefix is an error (BadLength or Truncated), never a panic.
        for n in 0..msg.len() {
            assert!(parse(&msg[..n]).and_then(|(_, p)| from_payload::<T>(p)).is_err(), "prefix {n}");
        }
        // One stray byte after the payload is caught.
        let mut p = payload.to_vec();
        p.push(0);
        assert_eq!(from_payload::<T>(&p).unwrap_err(), WireError::TrailingBytes);
    }
}

fn report() -> c::FaultReport {
    c::FaultReport {
        seq: 3,
        ts_ns: 1_234_567,
        pid: 9,
        service: "files".to_string(),
        kind: c::FaultKind::PageFault,
        rip: 0x40_1234,
        addr: 0,
        message: "A page fault (#PF) at 0x401234".to_string(),
        frames: vec![0x40_1234, 0x40_2000, 0x40_3000],
        restarted: true,
    }
}

#[test]
fn every_conductor_message_round_trips() {
    round_trip(c::CONNECT, c::ByName { name: "logger".to_string() });
    round_trip(c::CONNECT, c::Connected { channel: HandleRef(0) });
    round_trip(c::REGISTER, c::Register { name: "stage".to_string(), endpoint: HandleRef(0) });
    round_trip(c::LIST_SERVICES, ());
    for state in [
        c::ServiceState::Waiting,
        c::ServiceState::Starting,
        c::ServiceState::Running,
        c::ServiceState::Restarting,
        c::ServiceState::Held,
        c::ServiceState::Stopped,
    ] {
        round_trip(
            c::LIST_SERVICES,
            c::Services {
                services: vec![c::ServiceInfo {
                    name: "logger".to_string(),
                    description: "Keeps the system's log.".to_string(),
                    state,
                    pid: 3,
                    restarts: 1,
                    started_ns: 99,
                    binary_sha256: [0x5A; 32],
                    cpu_ns: 12_345_678,
                    mem_bytes: 6 << 20,
                }],
            },
        );
        round_trip(c::WATCH, c::ServiceEvent { name: "stage".to_string(), state });
    }
    round_trip(c::RESTART, c::ByName { name: "desktop".to_string() });
    round_trip(c::SWAP, c::Swap { name: "files".to_string(), binary: HandleRef(0), signature: [7; 64] });
    round_trip(c::SWAP, c::Swapped { old_sha256: [1; 32], new_sha256: [2; 32], ms: 140 });
    round_trip(c::ROLLBACK, c::ByName { name: "files".to_string() });
    round_trip(c::FAULTS, c::Page { cursor: 0, limit: 32 });
    for kind in [
        c::FaultKind::Panic,
        c::FaultKind::PageFault,
        c::FaultKind::GeneralProtection,
        c::FaultKind::InvalidOpcode,
        c::FaultKind::DivideError,
        c::FaultKind::Watchdog,
        c::FaultKind::Assert,
        c::FaultKind::IpcProtocol,
    ] {
        round_trip(c::FAULTS, c::Faults { reports: vec![c::FaultReport { kind, ..report() }], next_cursor: 4 });
    }
    round_trip(
        c::START,
        c::Start { name: "logger".to_string(), flavour: 2, handles: vec![("serial".to_string(), HandleRef(0))] },
    );
    round_trip(c::INCOMING, c::Incoming { client: "stage".to_string(), channel: HandleRef(0) });
    round_trip(c::BOOT_TIMELINE, c::BootTimeline { stages: vec![(2, 0), (6, 912_000_000), (10, 1_040_000_000)] });
}

#[test]
fn every_logger_message_round_trips() {
    round_trip(l::WRITE, l::Write { level: l::INFO, msg: "conductor: started".to_string() });
    round_trip(l::READ, l::Read { cursor: 0, limit: 100, filter: l::Filter { svc: None, min_level: None } });
    round_trip(
        l::READ,
        l::Read { cursor: 5, limit: 1, filter: l::Filter { svc: Some("stage".to_string()), min_level: Some(l::WARN) } },
    );
    round_trip(
        l::READ,
        l::Records {
            records: vec![l::LogRecord {
                seq: 1,
                ts_ns: 2,
                level: l::ERROR,
                svc: "x".to_string(),
                msg: "é ✓".to_string(),
            }],
            next_cursor: 0,
            newest: 9,
        },
    );
    round_trip(l::SUBSCRIBE, l::Filter { svc: None, min_level: Some(l::DEBUG) });
}

#[test]
fn error_replies_carry_a_code_and_a_message() {
    let msg = error_reply(9, c::CONNECT, -10, "no service called \"nope\"").unwrap();
    let (h, p) = parse(&msg).unwrap();
    assert!(h.is_reply() && h.is_error() && !h.is_event());
    assert_eq!(h.txid, 9);
    assert_eq!(error_of(p).unwrap(), (-10, "no service called \"nope\"".to_string()));
}

#[test]
fn the_header_is_sixteen_bytes_in_the_documented_order() {
    let h = Header { txid: 0x0403_0201, ordinal: 0x0807_0605, flags: 0x0A09 & flags::KNOWN, payload_len: 0x0C0B };
    let b = h.encode();
    assert_eq!(b, [1, 2, 3, 4, 5, 6, 7, 8, 0x01, 0x00, 0x0B, 0x0C, 0, 0, 0, 0]);
    assert_eq!(Header::decode(&b).unwrap(), h);
}

#[test]
fn malformed_messages_are_errors() {
    let mut m = request(1, 1, &c::ByName { name: "a".to_string() }).unwrap();
    // A reserved byte, an unknown flag, a wrong payload length.
    m[13] = 1;
    assert_eq!(parse(&m).unwrap_err(), WireError::Reserved);
    m[13] = 0;
    m[8] = 0x80;
    assert_eq!(parse(&m).unwrap_err(), WireError::Reserved);
    m[8] = 0;
    m[10] += 1;
    assert_eq!(parse(&m).unwrap_err(), WireError::BadLength);
    // Invalid UTF-8; an unknown enum tag; a bool that isn't 0 or 1; an option tag 2.
    assert_eq!(from_payload::<String>(&[2, 0, 0xFF, 0xFE]).unwrap_err(), WireError::BadUtf8);
    assert_eq!(from_payload::<c::FaultKind>(&[8]).unwrap_err(), WireError::BadTag);
    assert_eq!(from_payload::<bool>(&[2]).unwrap_err(), WireError::BadTag);
    assert_eq!(from_payload::<Option<u8>>(&[2, 0]).unwrap_err(), WireError::BadTag);
    // A list claiming 65,535 items in 2 bytes doesn't allocate for them.
    assert_eq!(from_payload::<Vec<u64>>(&[0xFF, 0xFF]).unwrap_err(), WireError::Truncated);
    // Too long to send.
    assert_eq!(request(1, 1, &vec![0u8; MAX_PAYLOAD]).unwrap_err(), WireError::TooLong);
}

#[test]
fn random_bytes_never_panic() {
    let iters: u32 = std::env::var("FREALLY_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(300);
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for _ in 0..iters * 10 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let n = (x % 200) as usize;
        let bytes: Vec<u8> = (0..n).map(|i| (x >> (i % 56)) as u8).collect();
        let _ = parse(&bytes);
        let _ = from_payload::<c::Faults>(&bytes);
        let _ = from_payload::<c::Services>(&bytes);
        let _ = from_payload::<l::Records>(&bytes);
    }
}

#[test]
fn the_app_doors_round_trip() {
    use crate::proto::{monitor as m, terminal as t};
    round_trip(
        m::STATE,
        m::State {
            tab: "Processes".to_string(),
            sort: "Name up".to_string(),
            filter: "all 0".to_string(),
            lines: vec!["stage | Running | 25.0 MiB | 2.0 % | 0".to_string()],
            window: 7,
        },
    );
    round_trip(t::FEED, t::Bytes { data: b"\x1b[31mred\x1b[0m".to_vec() });
    round_trip(
        t::SCREEN,
        t::Screen { rows: vec!["freally:~$ ".to_string()], cursor: (0, 11), scrollback: 3, cols: 96, window: 4 },
    );
}

#[test]
fn every_devmgr_message_round_trips() {
    use crate::proto::devmgr as d;
    round_trip(
        d::LIST,
        d::Devices {
            pci: vec![d::PciDevice {
                segment: 0,
                bus: 0,
                device: 31,
                function: 2,
                vendor: 0x8086,
                id: 0x2922,
                class: 1,
                subclass: 6,
                prog_if: 1,
            }],
        },
    );
    round_trip(d::TABLES, d::Tables { tables: vec![("APIC".to_string(), (0x3fb7_d000, 120))] });
    round_trip(d::FIRMWARE_TIMES, d::FirmwareTimes { reset_end: 1, os_loader_start: 2 });
    round_trip(
        d::RESOURCES,
        d::Resources {
            handles: vec![("framebuffer".to_string(), HandleRef(0))],
            values: vec![("width".to_string(), 1280)],
        },
    );
}
