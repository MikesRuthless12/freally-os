// SPDX-License-Identifier: Apache-2.0
//! drv-virtio-net (TASK-172): the first virtio network device devmgr finds,
//! driven with one receive and one transmit queue. Completions interrupt on
//! one MSI-X vector (polled every 10 ms without one). It serves the netdev
//! protocol (PRD § 4) to netstack alone, and to conductor's test door in e2e
//! images. With no device it says so and ends.
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use drv_virtio_net::{self as vn, Mem, Queue, common, netcfg, status};
use fcore_abi::{FErr, cache, perm, signal, syscall as nr, well_known};
use fstd::handle::Handle;
use fstd::ipc::{Channel, Client};
use fstd::time::{Duration, Instant};
use fwire::proto::conductor as c;
use fwire::proto::devmgr as d;
use fwire::proto::netdev as nd;

fstd::program!(run);

/// Each buffer: a header and the largest frame, rounded up.
const BUF: u64 = 2048;
/// Frames a subscriber may leave unread before the rest are dropped.
const BACKLOG: usize = 256;

/// Memory the device reaches by DMA: one physical run, mapped here.
struct Dma {
    _vmo: Handle,
    va: u64,
    phys: u64,
}

impl Dma {
    fn new(len: u64) -> Result<Dma, String> {
        let h = fstd::sys::vmo_create_contiguous(len).map_err(|e| alloc::format!("DMA memory: {}", e.describe()))?;
        let vmo = Handle::from_raw(h).ok_or("no VMO")?;
        let va = fstd::sys::vm_map(well_known::SELF_ASPACE, h, 0, len, 0, perm::READ | perm::WRITE)
            .map_err(|e| alloc::format!("mapping DMA memory: {}", e.describe()))?;
        let phys = fstd::sys::vmo_phys(h, 0).map_err(|e| alloc::format!("DMA memory's address: {}", e.describe()))?;
        // SAFETY: freshly mapped, ours.
        unsafe { core::ptr::write_bytes(va as *mut u8, 0, len as usize) };
        Ok(Dma { _vmo: vmo, va, phys })
    }
}

/// A queue's page, as [`vn::Queue`] reads and writes it.
struct Page(u64);

impl Mem for Page {
    fn r16(&self, o: usize) -> u16 {
        // SAFETY: inside the queue's mapped page; the device writes it.
        unsafe { core::ptr::read_volatile((self.0 + o as u64) as *const u16) }
    }
    fn r32(&self, o: usize) -> u32 {
        // SAFETY: as above.
        unsafe { core::ptr::read_volatile((self.0 + o as u64) as *const u32) }
    }
    fn w16(&mut self, o: usize, v: u16) {
        // SAFETY: as above.
        unsafe { core::ptr::write_volatile((self.0 + o as u64) as *mut u16, v) }
    }
    fn w32(&mut self, o: usize, v: u32) {
        // SAFETY: as above.
        unsafe { core::ptr::write_volatile((self.0 + o as u64) as *mut u32, v) }
    }
    fn w64(&mut self, o: usize, v: u64) {
        // SAFETY: as above.
        unsafe { core::ptr::write_volatile((self.0 + o as u64) as *mut u64, v) }
    }
    fn fence(&self) {
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    }
}

/// One queue: its ring's page, its buffers, and its doorbell.
struct Ring {
    q: Queue,
    page: Dma,
    bufs: Dma,
    index: u16,
    doorbell: u64,
}

impl Ring {
    fn buf(&self, id: u16) -> (u64, u64) {
        (self.bufs.va + id as u64 * BUF, self.bufs.phys + id as u64 * BUF)
    }

    fn ring_doorbell(&self) {
        // SAFETY: the doorbell, inside the notify structure's mapping.
        unsafe { fstd::mmio::write16(self.doorbell, self.index) }
    }
}

struct Net {
    device: u64,
    rx: Ring,
    tx: Ring,
    /// Transmit descriptors not with the device.
    tx_free: Vec<u16>,
    mac: [u8; 6],
    link: bool,
    status_known: bool,
    irq: Option<Handle>,
    stats: nd::Stats,
    /// Who subscribed (at most [`SUBSCRIBERS`]), and the frames received
    /// for them not yet sent.
    subs: Vec<Channel>,
    waiting: Vec<Vec<u8>>,
}

/// The most subscribers at once: netstack, and the test door beside it.
const SUBSCRIBERS: usize = 4;

fn value(r: &d::Resources, name: &str) -> u64 {
    r.values.iter().find(|(k, _)| k == name).map_or(0, |(_, v)| *v)
}

/// Maps `len` bytes of device memory (`resource` covers it), uncached.
fn map_mmio(resource: &Handle, phys: u64, len: u64) -> Result<u64, String> {
    let vmo = fstd::sys::vmo_create_physical(resource.raw(), phys, len, cache::UNCACHED)
        .map_err(|e| alloc::format!("vmo_create_physical: {}", e.describe()))?;
    let va = fstd::sys::vm_map(well_known::SELF_ASPACE, vmo, 0, len, 0, perm::READ | perm::WRITE)
        .map_err(|e| alloc::format!("vm_map: {}", e.describe()))?;
    let _ = fstd::sys::handle_close(vmo);
    Ok(va)
}

/// The common structure's fields.
fn c8(base: u64, off: u64) -> u8 {
    // SAFETY: inside the mapped common structure.
    unsafe { fstd::mmio::read8(base + off) }
}
fn c16(base: u64, off: u64) -> u16 {
    // SAFETY: as above.
    unsafe { fstd::mmio::read16(base + off) }
}
fn c32(base: u64, off: u64) -> u32 {
    // SAFETY: as above.
    unsafe { fstd::mmio::read32(base + off) }
}
fn w8(base: u64, off: u64, v: u8) {
    // SAFETY: as above.
    unsafe { fstd::mmio::write8(base + off, v) }
}
fn w16(base: u64, off: u64, v: u16) {
    // SAFETY: as above.
    unsafe { fstd::mmio::write16(base + off, v) }
}
fn w32(base: u64, off: u64, v: u32) {
    // SAFETY: as above.
    unsafe { fstd::mmio::write32(base + off, v) }
}
/// A 64-bit field, written as two 32-bit halves, low first (§ 4.1.3.1).
fn w64(base: u64, off: u64, v: u64) {
    w32(base, off, v as u32);
    w32(base, off + 4, (v >> 32) as u32);
}

/// What devmgr granted, mapped: each region's address in this process.
struct Granted {
    config: u64,
    common: u64,
    notify: u64,
    notify_mul: u32,
    device: u64,
    /// The MSI-X capability's offset and the table's entry 0, mapped.
    msix: Option<(u64, u64)>,
    /// A grant to bind the interrupt with.
    any: Handle,
}

fn grants() -> Result<Granted, String> {
    let devmgr = fstd::service::connect("devmgr").map_err(|e| alloc::format!("devmgr: {e:?}"))?;
    let mut client = Client::new(devmgr);
    let (r, handles) = client
        .call::<_, d::Resources>(d::RESOURCES, &(), Vec::new(), Instant::after(Duration::from_secs(2)))
        .map_err(|e| match e {
            fstd::ipc::CallError::Remote(code, _) if code == FErr::NotFound as i32 => String::from(NO_DEVICE),
            e => alloc::format!("devmgr's resources: {e:?}"),
        })?;
    let mut handles: Vec<Option<Handle>> = handles.into_iter().map(Some).collect();
    let mut take =
        |name: &str| r.handles.iter().find(|(n, _)| n == name).and_then(|(_, h)| handles.get_mut(h.0 as usize)?.take());
    let config_h = take("config").ok_or(NO_DEVICE)?;
    let config = map_mmio(&config_h, value(&r, "config"), 4096)?;
    // Each BAR's span, mapped once; a region's address is inside its span.
    let mut spans: Vec<(u64, u64, u64)> = Vec::new();
    let mut any = None;
    for bar in 0..6 {
        let name = alloc::format!("bar{bar}");
        if let Some(h) = take(&name) {
            let (base, len) = (value(&r, &name), value(&r, &alloc::format!("{name}-len")));
            spans.push((base, len, map_mmio(&h, base, len)?));
            any.get_or_insert(h);
        }
    }
    let at = |phys: u64| spans.iter().find(|(b, l, _)| phys >= *b && phys < b + l).map(|(b, _, va)| va + (phys - b));
    let region = |name: &str| {
        let phys = value(&r, name);
        (phys != 0).then(|| at(phys)).flatten().ok_or_else(|| alloc::format!("devmgr granted no {name} structure"))
    };
    let (common, notify, device) = (region("common")?, region("notify")?, region("device")?);
    let msix = r
        .values
        .iter()
        .any(|(k, _)| k == "msix-cap")
        .then(|| at(value(&r, "msix-table")).map(|t| (value(&r, "msix-cap"), t)))
        .flatten();
    let _keep = config_h;
    Ok(Granted {
        config,
        common,
        notify,
        notify_mul: value(&r, "notify-mul") as u32,
        device,
        msix,
        any: any.ok_or("devmgr granted no BAR")?,
    })
}

const NO_DEVICE: &str = "no virtio network device";

/// MSI-X on: one vector for both queues and the configuration, entry 0 of
/// the table; legacy INTx off. Its line and vector, or `None` (polled).
fn msix(g: &Granted) -> Option<(Handle, u16)> {
    let (cap, entry) = g.msix?;
    // SAFETY: offsets inside the function's 4 KiB configuration page, mapped.
    let rd16 = |o: u64| unsafe { fstd::mmio::read16(g.config + o) };
    // SAFETY: as above.
    let wr16 = |o: u64, v: u16| unsafe { fstd::mmio::write16(g.config + o, v) };
    // SAFETY: as above.
    if unsafe { fstd::mmio::read8(g.config + cap) } != 0x11 {
        return None;
    }
    let (h, message) = match fstd::sys::call(nr::INTERRUPT_BIND_MSI, [g.any.raw() as u64, 0, 0, 0, 0, 0]) {
        Ok(r) => r,
        Err(e) => {
            fstd::warn!("drv-virtio-net: no MSI-X vector ({}): polled", e.describe());
            return None;
        }
    };
    let line = Handle::from_raw(h as u32)?;
    // SAFETY: entry 0 of the table (16 bytes), mapped: address, data, unmasked.
    unsafe {
        fstd::mmio::write32(entry, message as u32);
        fstd::mmio::write32(entry + 4, 0);
        fstd::mmio::write32(entry + 8, (message >> 32) as u32);
        fstd::mmio::write32(entry + 12, 0);
    }
    let control = rd16(cap + 2);
    wr16(cap + 2, (control | 1 << 15) & !(1 << 14));
    wr16(4, rd16(4) | 1 << 10);
    Some((line, 0))
}

impl Net {
    /// The bring-up (§ 3.1.1): reset, acknowledge, the features agreed,
    /// both queues given their pages, the interrupt set, driver OK, and
    /// every receive buffer handed to the device.
    fn open() -> Result<Net, String> {
        let g = grants()?;
        // Memory space and bus mastering on (the command register).
        // SAFETY: the function's configuration page, mapped.
        unsafe { fstd::mmio::write16(g.config + 4, fstd::mmio::read16(g.config + 4) | 0b110) };
        let cm = g.common;
        w8(cm, common::DEVICE_STATUS, 0);
        let until = Instant::now().as_ns() + 1_000_000_000;
        while c8(cm, common::DEVICE_STATUS) != 0 {
            if Instant::now().as_ns() > until {
                return Err(String::from("the device never finished its reset"));
            }
            fstd::time::sleep(Duration::from_millis(1));
        }
        w8(cm, common::DEVICE_STATUS, status::ACKNOWLEDGE);
        w8(cm, common::DEVICE_STATUS, status::ACKNOWLEDGE | status::DRIVER);
        let mut offered = 0u64;
        for half in 0..2u32 {
            w32(cm, common::DEVICE_FEATURE_SELECT, half);
            offered |= (c32(cm, common::DEVICE_FEATURE) as u64) << (32 * half);
        }
        let agreed = match vn::negotiate(offered) {
            Ok(f) => f,
            Err(e) => {
                w8(cm, common::DEVICE_STATUS, status::FAILED);
                return Err(String::from(e));
            }
        };
        for half in 0..2u32 {
            w32(cm, common::DRIVER_FEATURE_SELECT, half);
            w32(cm, common::DRIVER_FEATURE, (agreed >> (32 * half)) as u32);
        }
        let s = status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK;
        w8(cm, common::DEVICE_STATUS, s);
        if c8(cm, common::DEVICE_STATUS) & status::FEATURES_OK == 0 {
            w8(cm, common::DEVICE_STATUS, status::FAILED);
            return Err(String::from("the device refused the features agreed"));
        }
        let irq = msix(&g);
        let vector = irq.as_ref().map_or(common::NO_VECTOR, |(_, v)| *v);
        w16(cm, common::CONFIG_MSIX_VECTOR, vector);
        let rx = Self::queue(&g, vn::RX, vector)?;
        let tx = Self::queue(&g, vn::TX, vector)?;
        w8(cm, common::DEVICE_STATUS, s | status::DRIVER_OK);
        let mut mac = [0u8; 6];
        if agreed & vn::feature::NET_MAC != 0 {
            for (i, b) in mac.iter_mut().enumerate() {
                // SAFETY: inside the mapped device structure.
                *b = unsafe { fstd::mmio::read8(g.device + netcfg::MAC + i as u64) };
            }
        }
        let tx_free = (0..tx.q.size).rev().collect();
        let mut n = Net {
            device: g.device,
            rx,
            tx,
            tx_free,
            mac,
            link: true,
            status_known: agreed & vn::feature::NET_STATUS != 0,
            irq: irq.map(|(h, _)| h),
            stats: nd::Stats { rx_frames: 0, rx_bytes: 0, rx_dropped: 0, tx_frames: 0, tx_bytes: 0, tx_dropped: 0 },
            subs: Vec::new(),
            waiting: Vec::new(),
        };
        n.link = n.read_link();
        for id in 0..n.rx.q.size {
            let (_, phys) = n.rx.buf(id);
            n.rx.q.post(&mut Page(n.rx.page.va), id, phys, BUF as u32, true);
        }
        n.rx.q.publish(&mut Page(n.rx.page.va));
        n.rx.ring_doorbell();
        Ok(n)
    }

    /// Queue `index` given its page, as many entries as one page holds (or
    /// fewer, if the device has fewer), and its MSI-X vector.
    fn queue(g: &Granted, index: u16, vector: u16) -> Result<Ring, String> {
        let cm = g.common;
        w16(cm, common::QUEUE_SELECT, index);
        let max = c16(cm, common::QUEUE_SIZE);
        if max == 0 {
            return Err(alloc::format!("the device has no queue {index}"));
        }
        let size = Queue::MAX.min(1 << (15 - max.leading_zeros()));
        let q = Queue::new(size).ok_or("a queue size that isn't a power of two")?;
        let page = Dma::new(4096)?;
        let bufs = Dma::new(size as u64 * BUF)?;
        w16(cm, common::QUEUE_SIZE, size);
        w64(cm, common::QUEUE_DESC, page.phys);
        w64(cm, common::QUEUE_DRIVER, page.phys + q.avail_off() as u64);
        w64(cm, common::QUEUE_DEVICE, page.phys + q.used_off() as u64);
        w16(cm, common::QUEUE_MSIX_VECTOR, vector);
        if vector != common::NO_VECTOR && c16(cm, common::QUEUE_MSIX_VECTOR) != vector {
            return Err(alloc::format!("queue {index} refused its MSI-X vector"));
        }
        let doorbell = g.notify + c16(cm, common::QUEUE_NOTIFY_OFF) as u64 * g.notify_mul as u64;
        w16(cm, common::QUEUE_ENABLE, 1);
        Ok(Ring { q, page, bufs, index, doorbell })
    }

    fn read_link(&self) -> bool {
        // SAFETY: inside the mapped device structure.
        !self.status_known || unsafe { fstd::mmio::read16(self.device + netcfg::STATUS) } & netcfg::LINK_UP != 0
    }

    /// Every frame the device has received, kept for the subscriber (or
    /// counted as dropped), and each buffer handed back.
    fn receive(&mut self) {
        let mut back = false;
        while let Some((id, len)) = self.rx.q.take(&Page(self.rx.page.va)) {
            let (va, phys) = self.rx.buf(id);
            // SAFETY: the buffer the device just finished with.
            let buf = unsafe { core::slice::from_raw_parts(va as *const u8, BUF as usize) };
            match vn::frame(buf, len) {
                Some(f) if !self.subs.is_empty() && self.waiting.len() < BACKLOG => {
                    self.stats.rx_frames += 1;
                    self.stats.rx_bytes += f.len() as u64;
                    self.waiting.push(f.to_vec());
                }
                _ => self.stats.rx_dropped += 1,
            }
            self.rx.q.post(&mut Page(self.rx.page.va), id, phys, BUF as u32, true);
            back = true;
        }
        if back {
            self.rx.q.publish(&mut Page(self.rx.page.va));
            self.rx.ring_doorbell();
        }
    }

    /// The transmit buffers the device is done with, free again.
    fn sent(&mut self) {
        while let Some((id, _)) = self.tx.q.take(&Page(self.tx.page.va)) {
            self.tx_free.push(id);
        }
    }

    /// Gives `frames` to the device in order, while it has room; how many.
    fn send(&mut self, frames: &[Vec<u8>]) -> Result<u32, (FErr, &'static str)> {
        if frames.iter().any(|f| f.len() < nd::FRAME_MIN || f.len() > nd::FRAME_MAX) {
            return Err((FErr::InvalidArgs, "a frame is 14 to 1514 bytes"));
        }
        self.sent();
        let mut n = 0u32;
        for f in frames {
            let Some(id) = self.tx_free.pop() else {
                self.stats.tx_dropped += (frames.len() - n as usize) as u64;
                break;
            };
            let (va, phys) = self.tx.buf(id);
            // SAFETY: a transmit buffer the device doesn't hold: a zero header, then the frame.
            unsafe {
                core::ptr::write_bytes(va as *mut u8, 0, vn::NET_HDR);
                core::ptr::copy_nonoverlapping(f.as_ptr(), (va + vn::NET_HDR as u64) as *mut u8, f.len());
            }
            self.tx.q.post(&mut Page(self.tx.page.va), id, phys, (vn::NET_HDR + f.len()) as u32, false);
            self.stats.tx_frames += 1;
            self.stats.tx_bytes += f.len() as u64;
            n += 1;
        }
        if n > 0 {
            self.tx.q.publish(&mut Page(self.tx.page.va));
            self.tx.ring_doorbell();
        }
        Ok(n)
    }

    /// What's waiting goes to every subscriber, a batch a message; a link
    /// that changed is said. A subscriber gone is forgotten.
    fn deliver(&mut self) {
        let link = self.read_link();
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

    fn info(&self) -> nd::Info {
        nd::Info { driver: String::from("drv-virtio-net"), mac: self.mac, mtu: 1500, link: self.link }
    }
}

/// Who may use the device: netstack; and conductor's test door in e2e images.
fn allowed(client: &str) -> bool {
    client == "netstack" || (cfg!(feature = "flavour-e2e") && matches!(client, "conductor" | "cdpd"))
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    let mut net = match Net::open() {
        Ok(n) => n,
        Err(e) if e == NO_DEVICE => {
            fstd::info!("drv-virtio-net: no virtio network device: nothing to do");
            return 0;
        }
        Err(e) => {
            fstd::error!("drv-virtio-net: {e}");
            return 1;
        }
    };
    let m = net.mac;
    fstd::info!(
        "drv-virtio-net: up, MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, link {}, {} ({}+{} entries)",
        m[0],
        m[1],
        m[2],
        m[3],
        m[4],
        m[5],
        if net.link { "up" } else { "down" },
        if net.irq.is_some() { "MSI-X" } else { "polled" },
        net.rx.q.size,
        net.tx.q.size
    );
    let Ok((listen, give)) = Channel::pair() else { return 1 };
    if fstd::service::register(give).is_err() {
        return 1;
    }
    let mut clients: Vec<Channel> = Vec::new();
    loop {
        let mut items = alloc::vec![[listen.0.raw(), signal::READABLE]];
        let irq_at = net.irq.as_ref().map(|h| {
            items.push([h.raw(), signal::SIGNALED]);
            items.len() - 1
        });
        let first = items.len();
        items.extend(clients.iter().map(|ch| [ch.0.raw(), signal::READABLE | signal::PEER_CLOSED]));
        // With an interrupt, a second's backstop; polled, 10 ms.
        let wait = if net.irq.is_some() { 1000 } else { 10 };
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
                            fstd::warn!("drv-virtio-net: {} may not use the network device", inc.client);
                        }
                    }
                }
            }
            Ok(k) if Some(k) == irq_at => {}
            Ok(k) if k >= first => {
                if !serve(&mut net, &clients[k - first]) {
                    clients.remove(k - first);
                }
            }
            Ok(_) | Err(FErr::TimedOut) => {}
            Err(_) => fstd::time::sleep(Duration::from_millis(10)),
        }
        if let Some(h) = &net.irq {
            let _ = fstd::sys::call1(nr::INTERRUPT_ACK, [h.raw() as u64, 0, 0, 0, 0, 0]);
        }
        net.receive();
        net.sent();
        net.deliver();
    }
}

/// Answers every request waiting from one client; false once it's gone.
fn serve(net: &mut Net, ch: &Channel) -> bool {
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
        let (reply, handles) = match h.ordinal {
            nd::INFO => (fwire::reply(h.txid, h.ordinal, &net.info()), Vec::new()),
            nd::SUBSCRIBE if net.subs.len() >= SUBSCRIBERS => {
                (fwire::error_reply(h.txid, h.ordinal, FErr::NoMemory as i32, "four subscribers already"), Vec::new())
            }
            nd::SUBSCRIBE => match Channel::pair() {
                Ok((mine, theirs)) => {
                    net.subs.push(mine);
                    (
                        fwire::reply(h.txid, h.ordinal, &nd::Subscribed { events: fwire::HandleRef(0) }),
                        alloc::vec![theirs.0],
                    )
                }
                Err(_) => (fwire::error_reply(h.txid, h.ordinal, FErr::NoMemory as i32, "no channel"), Vec::new()),
            },
            nd::SEND => match fwire::from_payload::<nd::Frames>(p).map_err(|_| (FErr::InvalidArgs, "unreadable")) {
                Ok(f) => match net.send(&f.frames) {
                    Ok(sent) => (fwire::reply(h.txid, h.ordinal, &nd::Sent { sent }), Vec::new()),
                    Err((e, why)) => (fwire::error_reply(h.txid, h.ordinal, e as i32, why), Vec::new()),
                },
                Err((e, why)) => (fwire::error_reply(h.txid, h.ordinal, e as i32, why), Vec::new()),
            },
            nd::STATS => (fwire::reply(h.txid, h.ordinal, &net.stats), Vec::new()),
            _ => (fwire::error_reply(h.txid, h.ordinal, FErr::NotSupported as i32, "not served"), Vec::new()),
        };
        if let Ok(m) = reply {
            let _ = ch.send(&m, handles);
        }
    }
}
