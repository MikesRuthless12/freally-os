// SPDX-License-Identifier: Apache-2.0
//! devmgr (TASK-041): finds the hardware and hands each driver exactly its
//! device's resources.
//!
//! - ACPI: the tables through read-only mappings (`facpi`, every checksum
//!   checked): MADT, FADT, HPET, MCFG, FPDT.
//! - PCIe: every function on every bus reachable from MCFG's ECAM, bridges
//!   followed.
//! - Drivers connect and ask for their resources; devmgr knows each one by
//!   the name conductor gave its connection, and carves only what that driver
//!   needs from the root resource.
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

mod hw;

use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::{FErr, signal};
use fstd::ipc::Channel;
use fstd::time::{Duration, Instant};
use fwire::proto::conductor as c;
use fwire::proto::devmgr as d;
use hw::Hardware;

fstd::program!(run);

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    let (Some(root), Some(bootinfo)) = (fstd::service::take_handle("devices"), fstd::service::take_handle("bootinfo"))
    else {
        fstd::error!("devmgr: conductor gave no root resource or BootInfo");
        return 1;
    };
    let hw = match Hardware::find(root, bootinfo) {
        Ok(h) => h,
        Err(e) => {
            fstd::error!("devmgr: {e}");
            return 1;
        }
    };
    fstd::info!(
        "devmgr: {} ACPI tables, {} PCI functions, {} CPUs, 8042 {}",
        hw.tables.len(),
        hw.pci.len(),
        hw.cpus,
        if hw.has_8042 { "present" } else { "absent" }
    );
    let Ok((listen, give)) = Channel::pair() else { return 1 };
    if fstd::service::register(give).is_err() {
        return 1;
    }
    let mut clients: Vec<(String, Channel)> = Vec::new();
    loop {
        let mut items = alloc::vec![[listen.0.raw(), signal::READABLE]];
        items.extend(clients.iter().map(|(_, ch)| [ch.0.raw(), signal::READABLE | signal::PEER_CLOSED]));
        match fstd::sys::wait_many(&items, Instant::after(Duration::from_secs(60))) {
            Ok(0) => {
                if let Ok(m) = listen.recv(0)
                    && let Ok((h, p)) = fwire::parse(&m.bytes)
                    && h.ordinal == c::INCOMING
                    && let Ok(inc) = fwire::from_payload::<c::Incoming>(p)
                    && let Some(ch) = m.handles.into_iter().next()
                {
                    clients.push((inc.client, Channel(ch)));
                }
            }
            Ok(k) => {
                let i = k - 1;
                if !serve(&hw, &clients[i].0, &clients[i].1) {
                    clients.remove(i);
                }
            }
            Err(FErr::TimedOut) => {}
            Err(_) => fstd::time::sleep(Duration::from_millis(10)),
        }
    }
}

/// Answers every request waiting from one client; false once it's gone.
fn serve(hw: &Hardware, who: &str, ch: &Channel) -> bool {
    loop {
        let mut bytes = alloc::vec![0u8; 1024];
        let mut hs = [0u32; 4];
        match fstd::sys::channel_read(ch.0.raw(), &mut bytes, &mut hs) {
            Ok((n, _)) => {
                let Ok((h, _)) = fwire::parse(&bytes[..n]) else { continue };
                let (reply, handles) = match h.ordinal {
                    d::LIST => (fwire::reply(h.txid, h.ordinal, &d::Devices { pci: hw.pci.clone() }), Vec::new()),
                    d::TABLES => (fwire::reply(h.txid, h.ordinal, &hw.table_list()), Vec::new()),
                    d::FIRMWARE_TIMES => (fwire::reply(h.txid, h.ordinal, &hw.firmware), Vec::new()),
                    d::RESOURCES => match hw.resources_for(who) {
                        Ok((r, hs)) => (fwire::reply(h.txid, h.ordinal, &r), hs),
                        Err(e) => (
                            fwire::error_reply(h.txid, h.ordinal, e as i32, "no resources for this driver"),
                            Vec::new(),
                        ),
                    },
                    _ => (fwire::error_reply(h.txid, h.ordinal, FErr::NotSupported as i32, "not served"), Vec::new()),
                };
                if let Ok(m) = reply {
                    let _ = ch.send(&m, handles);
                }
            }
            Err(FErr::ShouldWait) => return true,
            Err(_) => return false,
        }
    }
}
