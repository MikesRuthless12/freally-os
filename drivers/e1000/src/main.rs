// SPDX-License-Identifier: Apache-2.0
//! drv-e1000 (TASK-173): the first Intel e1000-family card devmgr finds
//! (82540EM, 82545EM, 82574L), in legacy descriptor mode with one receive
//! and one transmit ring of 128. Polled: every 10 ms while frames come,
//! 100 ms when quiet (the 82540EM has no MSI). It serves the netdev
//! protocol (PRD § 4) through `fnetdev`. With no card it says so and ends.
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use drv_e1000::{BUF, ENTRIES, RxRing, TxRing, ctrl, reg};
use fcore_abi::{FErr, cache, perm, well_known};
use fstd::handle::Handle;
use fstd::ipc::Client;
use fstd::time::{Duration, Instant};
use fwire::proto::devmgr as d;

fstd::program!(run);

const NO_CARD: &str = "no e1000 card";

/// Memory the card reaches by DMA: one physical run, mapped here.
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

    fn desc(&self, i: u16) -> [u8; 16] {
        let mut d = [0u8; 16];
        // SAFETY: descriptor `i` of the ring's mapped page; the card writes it.
        unsafe { core::ptr::copy_nonoverlapping((self.va + i as u64 * 16) as *const u8, d.as_mut_ptr(), 16) };
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        d
    }

    fn set_desc(&self, i: u16, d: &[u8; 16]) {
        // SAFETY: as above.
        unsafe { core::ptr::copy_nonoverlapping(d.as_ptr(), (self.va + i as u64 * 16) as *mut u8, 16) };
    }
}

struct Card {
    regs: u64,
    mac: [u8; 6],
    rx: Dma,
    rx_bufs: Dma,
    tx: Dma,
    tx_bufs: Dma,
    rxr: RxRing,
    txr: TxRing,
}

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

impl Card {
    fn r32(&self, off: u64) -> u32 {
        // SAFETY: inside the mapped registers.
        unsafe { fstd::mmio::read32(self.regs + off) }
    }

    fn w32(&self, off: u64, v: u32) {
        // SAFETY: inside the mapped registers.
        unsafe { fstd::mmio::write32(self.regs + off, v) }
    }

    /// devmgr's grants, the reset, the link set up, both rings given.
    fn open() -> Result<Card, String> {
        let devmgr = fstd::service::connect("devmgr").map_err(|e| alloc::format!("devmgr: {e:?}"))?;
        let mut client = Client::new(devmgr);
        let (r, handles) = client
            .call::<_, d::Resources>(d::RESOURCES, &(), Vec::new(), Instant::after(Duration::from_secs(2)))
            .map_err(|e| match e {
                fstd::ipc::CallError::Remote(code, _) if code == FErr::NotFound as i32 => String::from(NO_CARD),
                e => alloc::format!("devmgr's resources: {e:?}"),
            })?;
        let take = |name: &str| r.handles.iter().find(|(n, _)| n == name).map(|(_, h)| h.0 as usize);
        let (Some(ri), Some(ci)) = (take("registers"), take("config")) else { return Err(String::from(NO_CARD)) };
        let regs = map_mmio(&handles[ri], value(&r, "bar0"), reg::SIZE)?;
        let config = map_mmio(&handles[ci], value(&r, "config"), 4096)?;
        // Memory space and bus mastering on (the command register).
        // SAFETY: the function's configuration page, mapped.
        unsafe { fstd::mmio::write16(config + 4, fstd::mmio::read16(config + 4) | 0b110) };
        let mut c = Card {
            regs,
            mac: [0; 6],
            rx: Dma::new(4096)?,
            rx_bufs: Dma::new(ENTRIES as u64 * BUF)?,
            tx: Dma::new(4096)?,
            tx_bufs: Dma::new(ENTRIES as u64 * BUF)?,
            rxr: RxRing::default(),
            txr: TxRing::default(),
        };
        // Interrupts off, the card reset (§ 14.5), interrupts off again.
        c.w32(reg::IMC, u32::MAX);
        c.w32(reg::CTRL, c.r32(reg::CTRL) | ctrl::RST);
        let until = Instant::now().as_ns() + 100_000_000;
        fstd::time::sleep(Duration::from_millis(1));
        while c.r32(reg::CTRL) & ctrl::RST != 0 {
            if Instant::now().as_ns() > until {
                return Err(String::from("the card never finished its reset"));
            }
            fstd::time::sleep(Duration::from_millis(1));
        }
        c.w32(reg::IMC, u32::MAX);
        let _ = c.r32(reg::ICR);
        c.w32(reg::CTRL, (c.r32(reg::CTRL) | ctrl::SLU | ctrl::ASDE) & !ctrl::HOLD);
        c.mac = drv_e1000::mac_of(c.r32(reg::RAL0), c.r32(reg::RAH0)).ok_or("the card holds no MAC address")?;
        for i in 0..128 {
            c.w32(reg::MTA + 4 * i, 0);
        }
        // Receive: every buffer the card's, but one (§ 14.4).
        for i in 0..ENTRIES {
            c.rx.set_desc(i, &drv_e1000::rx_desc(c.rx_bufs.phys + i as u64 * BUF));
        }
        c.w32(reg::RDBAL, c.rx.phys as u32);
        c.w32(reg::RDBAH, (c.rx.phys >> 32) as u32);
        c.w32(reg::RDLEN, ENTRIES as u32 * 16);
        c.w32(reg::RDH, 0);
        c.w32(reg::RDT, RxRing::first_tail() as u32);
        c.w32(reg::RCTL, drv_e1000::RCTL);
        // Transmit (§ 14.5).
        c.w32(reg::TDBAL, c.tx.phys as u32);
        c.w32(reg::TDBAH, (c.tx.phys >> 32) as u32);
        c.w32(reg::TDLEN, ENTRIES as u32 * 16);
        c.w32(reg::TDH, 0);
        c.w32(reg::TDT, 0);
        c.w32(reg::TCTL, drv_e1000::TCTL);
        c.w32(reg::TIPG, drv_e1000::TIPG);
        Ok(c)
    }
}

impl fnetdev::Device for Card {
    fn name(&self) -> &'static str {
        "drv-e1000"
    }

    fn mac(&self) -> [u8; 6] {
        self.mac
    }

    fn link(&mut self) -> bool {
        self.r32(reg::STATUS) & drv_e1000::STATUS_LU != 0
    }

    fn receive(&mut self, each: &mut dyn FnMut(&[u8])) {
        let mut tail = None;
        while let Some((len, whole)) = drv_e1000::rx_done(&self.rx.desc(self.rxr.next)) {
            let i = self.rxr.next;
            let va = self.rx_bufs.va + i as u64 * BUF;
            if whole && (14..=1514).contains(&len) {
                // SAFETY: the buffer the card just filled, `len` ≤ BUF bytes.
                each(unsafe { core::slice::from_raw_parts(va as *const u8, len) });
            }
            self.rx.set_desc(i, &drv_e1000::rx_desc(self.rx_bufs.phys + i as u64 * BUF));
            tail = Some(self.rxr.advance());
        }
        if let Some(t) = tail {
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            self.w32(reg::RDT, t as u32);
        }
    }

    fn reclaim(&mut self) {
        let tx = &self.tx;
        self.txr.reclaim(|i| drv_e1000::tx_done(&tx.desc(i)));
    }

    fn send(&mut self, f: &[u8]) -> bool {
        let Some((at, _)) = self.txr.push() else { return false };
        let (va, phys) = (self.tx_bufs.va + at as u64 * BUF, self.tx_bufs.phys + at as u64 * BUF);
        // SAFETY: a transmit buffer the card doesn't hold.
        unsafe { core::ptr::copy_nonoverlapping(f.as_ptr(), va as *mut u8, f.len()) };
        self.tx.set_desc(at, &drv_e1000::tx_desc(phys, f.len() as u16));
        true
    }

    fn flush(&mut self) {
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.w32(reg::TDT, self.txr.tail as u32);
    }

    fn irq(&self) -> Option<u32> {
        None
    }
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    let mut card = match Card::open() {
        Ok(c) => c,
        Err(e) if e == NO_CARD => {
            fstd::info!("drv-e1000: no e1000 card: nothing to do");
            return 0;
        }
        Err(e) => {
            fstd::error!("drv-e1000: {e}");
            return 1;
        }
    };
    let m = card.mac;
    let up = fnetdev::Device::link(&mut card);
    fstd::info!(
        "drv-e1000: up, MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, link {}, polled ({}+{} entries)",
        m[0],
        m[1],
        m[2],
        m[3],
        m[4],
        m[5],
        if up { "up" } else { "down" },
        ENTRIES,
        ENTRIES
    );
    fnetdev::serve(card)
}
