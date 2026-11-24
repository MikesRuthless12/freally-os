// SPDX-License-Identifier: Apache-2.0
//! fnetdev (TASK-173): what every network driver serves, served one way:
//! the netdev protocol (PRD § 4) over a [`Device`]. The clients, the
//! subscribers (up to four, each sent every frame), the frames waiting for
//! them, the counts, and the loop. A driver brings only its hardware.
#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use fcore_abi::{FErr, signal, syscall as nr};
use fstd::ipc::Channel;
use fstd::time::{Duration, Instant};
use fwire::proto::conductor as c;
use fwire::proto::netdev as nd;

/// One network card, as a driver drives it.
pub trait Device {
    /// The driver's service name (`drv-virtio-net`).
    fn name(&self) -> &'static str;
    fn mac(&self) -> [u8; 6];
    /// Whether the link is up now.
    fn link(&mut self) -> bool;
    /// Each frame received since last asked, each buffer handed back.
    fn receive(&mut self, each: &mut dyn FnMut(&[u8]));
    /// The transmit buffers the device has finished with, free again.
    fn reclaim(&mut self);
    /// Gives `frame` to the device; false when it has no room.
    fn send(&mut self, frame: &[u8]) -> bool;
    /// After a run of sends: the device told (a doorbell, a tail).
    fn flush(&mut self);
    /// The interrupt line's handle, if it has one (else it's polled).
    fn irq(&self) -> Option<u32>;
}

/// Frames subscribers may leave unread before the rest are dropped.
const BACKLOG: usize = 256;
/// The most subscribers at once: netstack, and the test door beside it.
const SUBSCRIBERS: usize = 4;
/// Polled: every 10 ms while frames came in the last second, else 100 ms.
const POLL_BUSY_MS: u64 = 10;
const POLL_IDLE_MS: u64 = 100;

/// Who may use the device: netstack; conductor's test door and cdpd in e2e images.
fn allowed(client: &str) -> bool {
    client == "netstack" || (fstd::service::flavour() == 2 && matches!(client, "conductor" | "cdpd"))
}

struct Server<D: Device> {
    dev: D,
    link: bool,
    stats: nd::Stats,
    subs: Vec<Channel>,
    waiting: Vec<Vec<u8>>,
    /// When a frame last came in (ns).
    last_frame: u64,
}

impl<D: Device> Server<D> {
    fn receive(&mut self) {
        let (subs, waiting, stats) = (!self.subs.is_empty(), &mut self.waiting, &mut self.stats);
        let mut any = false;
        self.dev.receive(&mut |f| {
            any = true;
            if subs && waiting.len() < BACKLOG {
                stats.rx_frames += 1;
                stats.rx_bytes += f.len() as u64;
                waiting.push(f.to_vec());
            } else {
                stats.rx_dropped += 1;
            }
        });
        if any {
            self.last_frame = Instant::now().as_ns();
        }
    }

    fn send(&mut self, frames: &[Vec<u8>]) -> Result<u32, (FErr, &'static str)> {
        if frames.iter().any(|f| f.len() < nd::FRAME_MIN || f.len() > nd::FRAME_MAX) {
            return Err((FErr::InvalidArgs, "a frame is 14 to 1514 bytes"));
        }
        self.dev.reclaim();
        let mut n = 0u32;
        for f in frames {
            if !self.dev.send(f) {
                self.stats.tx_dropped += (frames.len() - n as usize) as u64;
                break;
            }
            self.stats.tx_frames += 1;
            self.stats.tx_bytes += f.len() as u64;
            n += 1;
        }
        if n > 0 {
            self.dev.flush();
        }
        Ok(n)
    }

    /// What's waiting goes to every subscriber, a batch a message; a link
    /// that changed is said. A subscriber gone is forgotten.
    fn deliver(&mut self) {
        let link = self.dev.link();
        let changed = link != self.link;
        self.link = link;
        if changed && let Ok(m) = fwire::event(nd::EV_LINK, &nd::Link { up: link }) {
            self.subs.retain(|s| !matches!(s.send(&m, Vec::new()), Err(FErr::PeerClosed)));
        }
        while !self.waiting.is_empty() {
            let (mut count, mut bytes) = (0, 0);
            for f in &self.waiting {
                if count == nd::BATCH_FRAMES || bytes + f.len() > nd::BATCH_BYTES {
                    break;
                }
                count += 1;
                bytes += f.len() + 2;
            }
            let batch = nd::Frames { frames: self.waiting.drain(..count).collect() };
            let Ok(m) = fwire::event(nd::EV_FRAMES, &batch) else { break };
            let dropped = &mut self.stats.rx_dropped;
            self.subs.retain(|s| match s.send(&m, Vec::new()) {
                Ok(()) => true,
                Err(FErr::PeerClosed) => false,
                // A subscriber that isn't reading: these are dropped, never waited for.
                Err(_) => {
                    *dropped += batch.frames.len() as u64;
                    true
                }
            });
        }
    }

    /// Answers every request waiting from one client; false once it's gone.
    fn serve(&mut self, ch: &Channel) -> bool {
        loop {
            let m = match ch.try_recv() {
                Ok(m) => m,
                Err(FErr::ShouldWait) => return true,
                Err(_) => return false,
            };
            if fstd::inventory::seen(ch, &m.bytes) {
                continue;
            }
            let Ok((h, p)) = fwire::parse(&m.bytes) else { continue };
            let err = |e: FErr, why: &str| (fwire::error_reply(h.txid, h.ordinal, e as i32, why), Vec::new());
            let (reply, handles) = match h.ordinal {
                nd::INFO => {
                    let info = nd::Info {
                        driver: alloc::string::String::from(self.dev.name()),
                        mac: self.dev.mac(),
                        mtu: 1500,
                        link: self.link,
                    };
                    (fwire::reply(h.txid, h.ordinal, &info), Vec::new())
                }
                nd::SUBSCRIBE if self.subs.len() >= SUBSCRIBERS => err(FErr::NoMemory, "four subscribers already"),
                nd::SUBSCRIBE => match Channel::pair() {
                    Ok((mine, theirs)) => {
                        self.subs.push(mine);
                        let r = nd::Subscribed { events: fwire::HandleRef(0) };
                        (fwire::reply(h.txid, h.ordinal, &r), alloc::vec![theirs.0])
                    }
                    Err(_) => err(FErr::NoMemory, "no channel"),
                },
                nd::SEND => match fwire::from_payload::<nd::Frames>(p) {
                    Ok(f) => match self.send(&f.frames) {
                        Ok(sent) => (fwire::reply(h.txid, h.ordinal, &nd::Sent { sent }), Vec::new()),
                        Err((e, why)) => err(e, why),
                    },
                    Err(_) => err(FErr::InvalidArgs, "unreadable"),
                },
                nd::STATS => (fwire::reply(h.txid, h.ordinal, &self.stats), Vec::new()),
                _ => err(FErr::NotSupported, "not served"),
            };
            if let Ok(m) = reply {
                let _ = ch.send(&m, handles);
            }
        }
    }
}

/// Registers the driver and serves netdev over `dev`, forever: woken by its
/// interrupt (a second's backstop), else polled (10 ms while frames come,
/// 100 ms when quiet). Returns only if registering fails.
pub fn serve<D: Device>(mut dev: D) -> i64 {
    let link = dev.link();
    let mut s = Server {
        dev,
        link,
        stats: nd::Stats { rx_frames: 0, rx_bytes: 0, rx_dropped: 0, tx_frames: 0, tx_bytes: 0, tx_dropped: 0 },
        subs: Vec::new(),
        waiting: Vec::new(),
        last_frame: 0,
    };
    let Ok((listen, give)) = Channel::pair() else { return 1 };
    if fstd::service::register(give).is_err() {
        return 1;
    }
    let mut clients: Vec<Channel> = Vec::new();
    loop {
        let mut items = alloc::vec![[listen.0.raw(), signal::READABLE]];
        let irq = s.dev.irq();
        if let Some(h) = irq {
            items.push([h, signal::SIGNALED]);
        }
        let first = items.len();
        items.extend(clients.iter().map(|ch| [ch.0.raw(), signal::READABLE | signal::PEER_CLOSED]));
        let busy = Instant::now().as_ns().saturating_sub(s.last_frame) < 1_000_000_000;
        let wait = match (irq, busy) {
            (Some(_), _) => 1000,
            (None, true) => POLL_BUSY_MS,
            (None, false) => POLL_IDLE_MS,
        };
        match fstd::sys::wait_many(&items, Instant::after(Duration::from_millis(wait))) {
            Ok(0) => {
                while let Ok(m) = listen.try_recv() {
                    if let Ok((h, p)) = fwire::parse(&m.bytes)
                        && h.ordinal == c::INCOMING
                        && let Ok(inc) = fwire::from_payload::<c::Incoming>(p)
                        && let Some(ch) = m.handles.into_iter().next()
                    {
                        if allowed(&inc.client) {
                            clients.push(Channel(ch));
                        } else {
                            fstd::warn!("{}: {} may not use the network device", s.dev.name(), inc.client);
                        }
                    }
                }
            }
            Ok(k) if k >= first => {
                if !s.serve(&clients[k - first]) {
                    clients.remove(k - first);
                }
            }
            Ok(_) | Err(FErr::TimedOut) => {}
            Err(_) => fstd::time::sleep(Duration::from_millis(10)),
        }
        if let Some(h) = irq {
            let _ = fstd::sys::call1(nr::INTERRUPT_ACK, [h as u64, 0, 0, 0, 0, 0]);
        }
        s.receive();
        s.dev.reclaim();
        s.deliver();
    }
}
