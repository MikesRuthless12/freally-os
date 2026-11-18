// SPDX-License-Identifier: Apache-2.0
//! drv-i8042 (TASK-043): the PS/2 controller, keyboard and mouse.
//!
//! - The controller: devices off, flushed, self-test (0x55), both ports tested,
//!   translation off (so the keyboard speaks scan code set 2), then IRQs 1
//!   and 12 on.
//! - The keyboard: reset, set 2, scanning on; bytes → `finput::Set2` → Freally
//!   key codes (HID usages) and the US layout's characters.
//! - The mouse: reset, defaults, IntelliMouse wheel detection (sample rates
//!   200, 100, 80, then its id is 3), reporting on.
//! - Subscribers (stage, from Phase 3) get every key and mouse event.
//! - No controller (the FADT says so, or nothing answers): *"No PS/2
//!   controller found."*, and a clean end (the manifest restarts failures only).
//!   A code no key has is ignored and logged once, at Debug.
//!
//! In e2e images each key and mouse report is also logged, for the
//! `@input.keys` and `@input.mouse` specs; never in release or dev, where the
//! log mustn't hold what you type.

#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::{FErr, signal, syscall as nr};
use fstd::handle::Handle;
use fstd::ipc::{CallError, Channel, Client};
use fstd::time::{Duration, Instant};
use fwire::proto::conductor as c;
use fwire::proto::devmgr as d;
use fwire::proto::input as inp;

fstd::program!(run);

const DATA: u16 = 0x60;
const STATUS: u16 = 0x64;
const OUT_FULL: u8 = 1;
const IN_FULL: u8 = 2;
const AUX: u8 = 0x20;

fn wait_write() -> bool {
    let t = Instant::now();
    while fstd::sys::inb(STATUS) & IN_FULL != 0 {
        if t.elapsed() > Duration::from_millis(100) {
            return false;
        }
    }
    true
}

fn command(cmd: u8) {
    if wait_write() {
        fstd::sys::outb(STATUS, cmd);
    }
}

fn write(b: u8) {
    if wait_write() {
        fstd::sys::outb(DATA, b);
    }
}

/// The next byte the controller has, within `ms`.
fn read(ms: u64) -> Option<u8> {
    let t = Instant::now();
    while fstd::sys::inb(STATUS) & OUT_FULL == 0 {
        if t.elapsed() > Duration::from_millis(ms) {
            return None;
        }
    }
    Some(fstd::sys::inb(DATA))
}

/// A command to a device: `aux` for the mouse (port 2). True if it acknowledged.
fn device(aux: bool, b: u8) -> bool {
    if aux {
        command(0xD4);
    }
    write(b);
    read(500) == Some(0xFA)
}

/// Why the driver can't run.
enum Fail {
    /// No PS/2 controller: the FADT says so (devmgr has none to give), or
    /// nothing answers. Not a failure: many machines have none.
    Absent,
    Broken(String),
}

impl From<&str> for Fail {
    fn from(s: &str) -> Fail {
        Fail::Broken(s.into())
    }
}

/// The controller and both devices, set up; whether the mouse has a wheel.
fn init() -> Result<bool, Fail> {
    // Ports nothing decodes read as all ones.
    if fstd::sys::inb(STATUS) == 0xFF {
        return Err(Fail::Absent);
    }
    command(0xAD);
    command(0xA7);
    while read(5).is_some() {}
    command(0x20);
    let mut cfg = read(100).ok_or(Fail::Absent)?;
    cfg &= !(1 | 2 | 0x40); // no IRQs yet; translation off (set 2 as the keyboard sends it)
    command(0x60);
    write(cfg);
    command(0xAA);
    if read(500) != Some(0x55) {
        return Err("the controller's self-test failed".into());
    }
    // Some controllers reset their configuration on self-test: write it again.
    command(0x60);
    write(cfg);
    command(0xAB);
    if read(100) != Some(0) {
        return Err("the keyboard port's test failed".into());
    }
    command(0xA9);
    let mouse_port = read(100) == Some(0);
    command(0xAE);
    if mouse_port {
        command(0xA8);
    }
    // The keyboard: reset (an ack, then 0xAA), set 2, scanning on.
    if !device(false, 0xFF) || read(1000) != Some(0xAA) {
        return Err("the keyboard didn't reset".into());
    }
    if !(device(false, 0xF0) && device(false, 0x02) && device(false, 0xF4)) {
        return Err("the keyboard refused scan code set 2".into());
    }
    let mut wheel = false;
    if mouse_port && device(true, 0xFF) && read(1000) == Some(0xAA) {
        let _ = read(100); // the mouse's id after reset (0)
        let _ = device(true, 0xF6);
        for rate in [200u8, 100, 80] {
            let _ = device(true, 0xF3) && device(true, rate);
        }
        if device(true, 0xF2) {
            wheel = read(100) == Some(3);
        }
        let _ = device(true, 0xF4);
    }
    while read(5).is_some() {}
    // `cfg` was read with both ports off (bits 4 and 5): turn them on, with their IRQs.
    let (ports_off, irqs) = if mouse_port { (0x30, 0x03) } else { (0x10, 0x01) };
    command(0x60);
    write((cfg & !ports_off) | irqs);
    Ok(wheel)
}

struct Resources {
    keyboard_irq: Handle,
    mouse_irq: Handle,
    gsis: [(u64, u64); 2],
}

fn resources() -> Result<Resources, Fail> {
    let devmgr = fstd::service::connect("devmgr").map_err(|e| Fail::Broken(alloc::format!("devmgr: {e:?}")))?;
    let deadline = Instant::after(Duration::from_secs(2));
    let (r, handles) = match Client::new(devmgr).call::<_, d::Resources>(d::RESOURCES, &(), Vec::new(), deadline) {
        Ok(x) => x,
        Err(CallError::Remote(code, _)) if code == FErr::NotFound as i32 => return Err(Fail::Absent),
        Err(e) => return Err(Fail::Broken(alloc::format!("devmgr's resources: {e:?}"))),
    };
    let mut by_name: Vec<(String, Handle)> = r.handles.iter().map(|(n, _)| n.clone()).zip(handles).collect();
    let mut take = |name: &str| {
        by_name
            .iter()
            .position(|(n, _)| n == name)
            .map(|i| by_name.swap_remove(i).1)
            .ok_or_else(|| Fail::Broken(alloc::format!("devmgr gave no {name}")))
    };
    let (data, cmd) = (take("data")?, take("command")?);
    let grant = |h: &Handle, port: u16| {
        fstd::sys::ioport_grant(h.raw(), port, 1)
            .map_err(|e| Fail::Broken(alloc::format!("port {port:#x}: {}", e.describe())))
    };
    grant(&data, DATA)?;
    grant(&cmd, STATUS)?;
    let v = |k: &str| r.values.iter().find(|(n, _)| n == k).map_or(0, |(_, x)| *x);
    Ok(Resources {
        keyboard_irq: take("keyboard-irq")?,
        mouse_irq: take("mouse-irq")?,
        gsis: [(v("keyboard-irq-gsi"), v("keyboard-irq-flags")), (v("mouse-irq-gsi"), v("mouse-irq-flags"))],
    })
}

fn bind(res: &Handle, (gsi, flags): (u64, u64)) -> Result<Handle, FErr> {
    let h = fstd::sys::call1(nr::INTERRUPT_BIND, [res.raw() as u64, gsi, flags, 0, 0, 0])?;
    Handle::from_raw(h as u32).ok_or(FErr::Internal)
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    let (res, wheel) = match resources().and_then(|r| init().map(|w| (r, w))) {
        Ok(x) => x,
        // Ending cleanly: the manifest restarts only a failure.
        Err(Fail::Absent) => {
            fstd::info!("drv-i8042: No PS/2 controller found.");
            return 0;
        }
        Err(Fail::Broken(e)) => {
            fstd::error!("drv-i8042: {e}");
            return 1;
        }
    };
    let (Ok(kbd), Ok(mouse)) = (bind(&res.keyboard_irq, res.gsis[0]), bind(&res.mouse_irq, res.gsis[1])) else {
        fstd::error!("drv-i8042: can't bind IRQs 1 and 12");
        return 1;
    };
    fstd::info!("drv-i8042: keyboard on set 2; mouse {}", if wheel { "with a wheel" } else { "without a wheel" });
    let Ok((listen, give)) = Channel::pair() else { return 1 };
    if fstd::service::register(give).is_err() {
        return 1;
    }
    let mut set2 = finput::Set2::default();
    // Codes no key has: each logged once, at Debug (bit: after 0xE0 × 256 + code).
    let mut unknown_told = [0u64; 8];
    let mut mods = finput::Modifiers::default();
    let mut packets = finput::Mouse::new(wheel);
    let mut subscribers: Vec<Channel> = Vec::new();
    let mut pending: Vec<Channel> = Vec::new();
    loop {
        let mut items = alloc::vec![
            [kbd.raw(), signal::SIGNALED],
            [mouse.raw(), signal::SIGNALED],
            [listen.0.raw(), signal::READABLE]
        ];
        items.extend(pending.iter().map(|ch| [ch.0.raw(), signal::READABLE | signal::PEER_CLOSED]));
        match fstd::sys::wait_many(&items, u64::MAX) {
            Ok(0) | Ok(1) => {
                // Everything the controller holds, whichever line rang.
                while fstd::sys::inb(STATUS) & OUT_FULL != 0 {
                    let aux = fstd::sys::inb(STATUS) & AUX != 0;
                    let b = fstd::sys::inb(DATA);
                    let mut out: Vec<Vec<u8>> = Vec::new();
                    if aux {
                        if let Some(m) = packets.feed(b) {
                            #[cfg(feature = "flavour-e2e")]
                            fstd::info!(
                                "drv-i8042: mouse dx {} dy {} wheel {} buttons {}",
                                m.dx,
                                m.dy,
                                m.wheel,
                                m.buttons
                            );
                            if let Ok(e) = fwire::event(
                                inp::MOUSE,
                                &inp::Mouse { dx: m.dx, dy: m.dy, wheel: m.wheel, buttons: m.buttons },
                            ) {
                                out.push(e);
                            }
                        }
                    } else {
                        let keys = [set2.feed(b), set2.pending()];
                        if let Some((ext, code)) = set2.take_unknown() {
                            let bit = (ext as usize) << 8 | code as usize;
                            if unknown_told[bit / 64] & 1 << (bit % 64) == 0 {
                                unknown_told[bit / 64] |= 1 << (bit % 64);
                                fstd::debug!(
                                    "drv-i8042: ignoring scan code {}{code:#04x} (no key has it)",
                                    if ext { "0xe0 " } else { "" }
                                );
                            }
                        }
                        for k in keys.into_iter().flatten() {
                            mods.update(k);
                            let ch = finput::us_char(k.usage, mods).map_or(0, |c| c as u32);
                            #[cfg(feature = "flavour-e2e")]
                            fstd::info!(
                                "drv-i8042: key {:#04x} {} U+{:04X}",
                                k.usage,
                                if k.down { "down" } else { "up" },
                                ch
                            );
                            if let Ok(e) = fwire::event(
                                inp::KEY,
                                &inp::Key { usage: k.usage, down: k.down, ch, modifiers: mods.bits() },
                            ) {
                                out.push(e);
                            }
                        }
                    }
                    for e in &out {
                        subscribers.retain(|s| s.send(e, Vec::new()).is_ok());
                    }
                }
                let _ = fstd::sys::call1(nr::INTERRUPT_ACK, [kbd.raw() as u64, 0, 0, 0, 0, 0]);
                let _ = fstd::sys::call1(nr::INTERRUPT_ACK, [mouse.raw() as u64, 0, 0, 0, 0, 0]);
            }
            Ok(2) => {
                if let Ok(m) = listen.recv(0)
                    && let Ok((h, _)) = fwire::parse(&m.bytes)
                    && h.ordinal == c::INCOMING
                    && let Some(ch) = m.handles.into_iter().next()
                {
                    pending.push(Channel(ch));
                }
            }
            Ok(k) => {
                // A new client's first message: subscribe.
                let ch = pending.remove(k - 3);
                if let Ok(m) = ch.recv(0)
                    && let Ok((h, _)) = fwire::parse(&m.bytes)
                    && h.ordinal == inp::SUBSCRIBE
                    && let Ok(r) = fwire::reply(h.txid, h.ordinal, &())
                    && ch.send(&r, Vec::new()).is_ok()
                {
                    subscribers.push(ch);
                }
            }
            Err(_) => fstd::time::sleep(Duration::from_millis(10)),
        }
    }
}
