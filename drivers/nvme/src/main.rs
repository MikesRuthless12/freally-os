// SPDX-License-Identifier: Apache-2.0
//! drv-nvme (TASK-092): the first NVMe controller devmgr finds, driven with
//! one admin queue and one I/O queue pair, polled (interrupts come with
//! MSI-X). It reads its disk's GPT when it starts, and every write passes
//! the partition interlock first: only inside Freally OS's own partitions.
//!
//! In e2e images it proves itself as it starts: a write to block 0 (the
//! protective MBR) is refused with `AccessDenied` before it reaches the
//! controller, and a block written inside FREALLY_DATA reads back the same.
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use drv_nvme::interlock::Interlock;
use drv_nvme::{Command, Completion, Ring, cq_doorbell, reg, sq_doorbell};
use fcore_abi::{FErr, cache, perm, well_known};
use fstd::handle::Handle;
use fstd::ipc::Client;
use fstd::time::{Duration, Instant};
use fwire::proto::devmgr as d;

fstd::program!(run);

/// Entries in each queue.
const ENTRIES: u16 = 32;
/// The data buffer: 128 KiB (32 pages), the most one command moves.
const BUF: u64 = 128 << 10;

/// Memory the controller reaches by DMA: one physical run, mapped here.
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
        Ok(Dma { _vmo: vmo, va, phys })
    }
}

struct Queue {
    qid: u16,
    sq: Dma,
    cq: Dma,
    ring: Ring,
}

struct Nvme {
    regs: u64,
    cap: u64,
    admin: Queue,
    io: Option<Queue>,
    buf: Dma,
    list: Dma,
    block: u32,
    blocks: u64,
    interlock: Interlock,
    /// FREALLY_DATA's last block, if the disk has one.
    data_last: Option<u64>,
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

impl Nvme {
    fn r32(&self, off: usize) -> u32 {
        // SAFETY: inside the mapped registers.
        unsafe { core::ptr::read_volatile((self.regs + off as u64) as *const u32) }
    }

    fn w32(&self, off: usize, v: u32) {
        // SAFETY: inside the mapped registers.
        unsafe { core::ptr::write_volatile((self.regs + off as u64) as *mut u32, v) }
    }

    fn w64(&self, off: usize, v: u64) {
        self.w32(off, v as u32);
        self.w32(off + 4, (v >> 32) as u32);
    }

    /// Waits for CSTS.RDY to be `ready`, up to the controller's own timeout.
    fn wait_ready(&self, ready: bool) -> Result<(), String> {
        let until = Instant::now().as_ns() + drv_nvme::ready_timeout_ms(self.cap) * 1_000_000;
        loop {
            let csts = self.r32(reg::CSTS);
            if csts & 2 != 0 {
                return Err(String::from("the controller reports a fatal status"));
            }
            if (csts & 1 != 0) == ready {
                return Ok(());
            }
            if Instant::now().as_ns() > until {
                return Err(alloc::format!("the controller never became {}", if ready { "ready" } else { "idle" }));
            }
            fstd::time::sleep(Duration::from_millis(1));
        }
    }

    /// Submits `cmd` on the admin queue (or the I/O queue) and polls for its
    /// completion, up to 5 s.
    fn run(&mut self, io: bool, mut cmd: Command) -> Result<Completion, String> {
        let (regs, cap) = (self.regs, self.cap);
        let q = if io { self.io.as_mut().ok_or("no I/O queue")? } else { &mut self.admin };
        let (slot, cid) = q.ring.submit();
        cmd.cid = cid;
        let bytes = cmd.bytes();
        // SAFETY: slot < ENTRIES, inside the submission queue's page.
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), (q.sq.va + slot as u64 * 64) as *mut u8, 64) };
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        // SAFETY: the doorbell is inside the mapped registers.
        unsafe {
            core::ptr::write_volatile((regs + sq_doorbell(cap, q.qid) as u64) as *mut u32, q.ring.sq_tail as u32)
        };
        let until = Instant::now().as_ns() + 5_000_000_000;
        let mut spins = 0u32;
        loop {
            let mut raw = [0u8; 16];
            // SAFETY: the head is inside the completion queue's page; the controller writes it.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    (q.cq.va + q.ring.cq_head as u64 * 16) as *const u8,
                    raw.as_mut_ptr(),
                    16,
                )
            };
            let c = Completion::parse(&raw);
            if let Some(head) = q.ring.consume(&c) {
                // SAFETY: as above.
                unsafe { core::ptr::write_volatile((regs + cq_doorbell(cap, q.qid) as u64) as *mut u32, head as u32) };
                if c.cid != cid {
                    continue;
                }
                return if c.ok() {
                    Ok(c)
                } else {
                    Err(alloc::format!("command {:#x} failed: status {:#x}", cmd.opcode, c.status))
                };
            }
            if Instant::now().as_ns() > until {
                return Err(alloc::format!("command {:#x} never completed", cmd.opcode));
            }
            spins += 1;
            if spins.is_multiple_of(256) {
                fstd::time::sleep(Duration::from_millis(1));
            } else {
                core::hint::spin_loop();
            }
        }
    }

    /// The PRP entries for `len` bytes of the data buffer.
    fn prp(&mut self, len: u64) -> Result<(u64, u64), String> {
        let pages: Vec<u64> = (0..BUF / 4096).map(|i| self.buf.phys + i * 4096).collect();
        // SAFETY: the list page is ours, 4 KiB: 512 entries.
        let list = unsafe { &mut *(self.list.va as *mut [u64; 512]) };
        drv_nvme::prps(&pages, 0, len, list, self.list.phys).ok_or_else(|| String::from("a transfer too large"))
    }

    /// Reads `count` blocks from `lba` into the data buffer.
    fn read(&mut self, lba: u64, count: u32) -> Result<&[u8], String> {
        let len = count as u64 * self.block as u64;
        if len == 0 || len > BUF || lba.checked_add(count as u64).is_none_or(|e| e > self.blocks) {
            return Err(String::from("a read outside the disk or larger than the buffer"));
        }
        let prp = self.prp(len)?;
        self.run(true, Command::rw(false, 1, lba, count, prp))?;
        // SAFETY: `len` bytes of the buffer, just read.
        Ok(unsafe { core::slice::from_raw_parts(self.buf.va as *const u8, len as usize) })
    }

    // The block protocol's callers (FreallyFS, TASK-094) arrive next; until
    // then the e2e image's proof is the one.
    #[cfg_attr(not(feature = "flavour-e2e"), allow(dead_code))]
    /// Writes `data` (whole blocks) at `lba`: the interlock first, then the
    /// controller. A refusal never reaches the device.
    fn write(&mut self, lba: u64, data: &[u8]) -> Result<(), FErr> {
        let count = data.len() as u64 / self.block as u64;
        if data.is_empty() || !(data.len() as u64).is_multiple_of(self.block as u64) || data.len() as u64 > BUF {
            return Err(FErr::InvalidArgs);
        }
        self.interlock.check_write(lba, count)?;
        // SAFETY: `data` fits the buffer.
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), self.buf.va as *mut u8, data.len()) };
        let prp = self.prp(data.len() as u64).map_err(|_| FErr::InvalidArgs)?;
        self.run(true, Command::rw(true, 1, lba, count as u32, prp)).map(|_| ()).map_err(|e| {
            fstd::error!("drv-nvme: {e}");
            FErr::Internal
        })
    }

    #[cfg_attr(not(feature = "flavour-e2e"), allow(dead_code))]
    fn flush(&mut self) -> Result<(), String> {
        self.run(true, Command::flush(1)).map(|_| ())
    }
}

fn open() -> Result<Nvme, String> {
    let devmgr = fstd::service::connect("devmgr").map_err(|e| alloc::format!("devmgr: {e:?}"))?;
    let mut client = Client::new(devmgr);
    let (r, handles) = client
        .call::<_, d::Resources>(d::RESOURCES, &(), Vec::new(), Instant::after(Duration::from_secs(2)))
        .map_err(|e| alloc::format!("devmgr's resources: {e:?}"))?;
    let take = |name: &str| r.handles.iter().find(|(n, _)| n == name).map(|(_, h)| h.0 as usize);
    let (Some(ri), Some(ci)) = (take("registers"), take("config")) else {
        return Err(String::from("devmgr found no NVMe controller"));
    };
    let regs = map_mmio(&handles[ri], value(&r, "bar0"), 0x2000)?;
    let config = map_mmio(&handles[ci], value(&r, "config"), 4096)?;
    // Memory space and bus mastering on (the command register, offset 4).
    // SAFETY: the function's configuration page, mapped.
    unsafe {
        let cmd = (config + 4) as *mut u16;
        core::ptr::write_volatile(cmd, core::ptr::read_volatile(cmd) | 0b110);
    }
    // SAFETY: inside the mapped registers.
    let cap = unsafe { core::ptr::read_volatile(regs as *const u64) };
    let entries = ENTRIES.min(drv_nvme::max_entries(cap).min(u16::MAX as u32) as u16);
    let queue = |qid: u16| -> Result<Queue, String> {
        Ok(Queue { qid, sq: Dma::new(4096)?, cq: Dma::new(4096)?, ring: Ring::new(entries) })
    };
    let mut n = Nvme {
        regs,
        cap,
        admin: queue(0)?,
        io: None,
        buf: Dma::new(BUF)?,
        list: Dma::new(4096)?,
        block: 512,
        blocks: 0,
        interlock: Interlock::read_only(),
        data_last: None,
    };
    // Reset, the admin queues, enable (§ 7.6.1).
    if n.r32(reg::CC) & 1 != 0 {
        n.w32(reg::CC, 0);
    }
    n.wait_ready(false)?;
    let size = (entries - 1) as u32;
    n.w32(reg::AQA, size << 16 | size);
    n.w64(reg::ASQ, n.admin.sq.phys);
    n.w64(reg::ACQ, n.admin.cq.phys);
    n.w32(reg::CC, drv_nvme::CC_ENABLE);
    n.wait_ready(true)?;
    let page = Dma::new(4096)?;
    n.run(false, Command::identify(1, 0, page.phys))?;
    // SAFETY: the page the controller just filled.
    let id = unsafe { core::slice::from_raw_parts(page.va as *const u8, 4096) };
    let (model, serial, fw) = drv_nvme::identify_controller(id);
    let (model, serial, fw) = (String::from(model), String::from(serial), String::from(fw));
    n.run(false, Command::identify(0, 1, page.phys))?;
    // SAFETY: as above.
    let ns = unsafe { core::slice::from_raw_parts(page.va as *const u8, 4096) };
    let (blocks, block) = drv_nvme::identify_namespace(ns)
        .ok_or("namespace 1 is inactive or has a block size the driver doesn't take")?;
    (n.blocks, n.block) = (blocks, block);
    // One I/O queue pair: completions first.
    let io = queue(1)?;
    n.run(false, Command::create_cq(1, entries as u32, io.cq.phys))?;
    n.run(false, Command::create_sq(1, entries as u32, io.sq.phys))?;
    n.io = Some(io);
    fstd::info!(
        "drv-nvme: {model} (serial {serial}, firmware {fw}): {blocks} blocks of {block} bytes, {entries}-entry queues"
    );
    (n.interlock, n.data_last) = read_gpt(&mut n);
    Ok(n)
}

/// The disk's GPT (the primary, else the backup) as the interlock, and
/// FREALLY_DATA's last block; a disk without one is read-only.
fn read_gpt(n: &mut Nvme) -> (Interlock, Option<u64>) {
    let bs = n.block as u64;
    let last = n.blocks - 1;
    for lba in [1, last] {
        let Ok(head) = n.read(lba, 1).map(|b| b.to_vec()) else { continue };
        let Ok(h) = fgpt::Header::parse(&head, lba, last) else { continue };
        let count = (h.entries_len() as u64).div_ceil(bs) as u32;
        let Ok(entries) = n.read(h.entries_lba, count).map(|b| b.to_vec()) else { continue };
        let Ok(t) = fgpt::Table::new(h, &entries) else { continue };
        for p in t.partitions() {
            let len = p.name.iter().position(|&u| u == 0).unwrap_or(p.name.len());
            let name = String::from_utf16_lossy(&p.name[..len]);
            fstd::info!(
                "drv-nvme: partition {name}: blocks {} to {} ({} MiB)",
                p.first_lba,
                p.last_lba,
                (p.blocks() * bs) >> 20
            );
        }
        let i = Interlock::from_table(&t);
        if i.is_read_only() {
            fstd::info!("drv-nvme: no Freally partition: the disk is read-only");
        }
        let data = t.partitions().find(|p| p.type_guid == fgpt::PART_TYPE_FREALLY_DATA).map(|p| p.last_lba);
        return (i, data);
    }
    fstd::info!("drv-nvme: no GPT: the disk is read-only");
    (Interlock::read_only(), None)
}

/// The e2e image's proof at start: the interlock refuses block 0 before the
/// controller sees it, and a block inside FREALLY_DATA is written, flushed
/// and read back the same.
#[cfg(feature = "flavour-e2e")]
fn prove(n: &mut Nvme) {
    let block = alloc::vec![0xA5u8; n.block as usize];
    match n.write(0, &block) {
        Err(FErr::AccessDenied) => {
            fstd::info!("drv-nvme: e2e: a write to block 0 (the protective MBR) refused: AccessDenied")
        }
        other => fstd::error!("drv-nvme: e2e: a write to block 0 wasn't refused: {other:?}"),
    }
    let Some(lba) = n.data_last else {
        fstd::info!("drv-nvme: e2e: no FREALLY_DATA to write in");
        return;
    };
    let pattern: Vec<u8> = (0..n.block).map(|i| (i * 7 + 3) as u8).collect();
    let ok =
        n.write(lba, &pattern).is_ok() && n.flush().is_ok() && n.read(lba, 1).is_ok_and(|b| b == pattern.as_slice());
    if ok {
        fstd::info!("drv-nvme: e2e: block {lba} of FREALLY_DATA written, flushed and read back the same");
    } else {
        fstd::error!("drv-nvme: e2e: block {lba} of FREALLY_DATA didn't read back as written");
    }
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    let n = match open() {
        Ok(n) => n,
        Err(e) => {
            fstd::error!("drv-nvme: {e}");
            return 1;
        }
    };
    hold(n)
}

/// Holds the controller: the e2e image proves it first.
fn hold(n: Nvme) -> i64 {
    #[cfg(feature = "flavour-e2e")]
    let n = {
        let mut n = n;
        prove(&mut n);
        n
    };
    let _held = n;
    // Callers (FreallyFS, TASK-094) come with the block protocol; until then
    // the driver holds the controller and waits.
    loop {
        fstd::time::sleep(Duration::from_secs(3600));
    }
}
