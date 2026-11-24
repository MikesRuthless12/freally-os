// SPDX-License-Identifier: Apache-2.0
//! drv-nvme (TASK-092): the first NVMe controller devmgr finds, driven with
//! one admin queue and one I/O queue pair, polled (interrupts come with
//! MSI-X). It reads its disk's GPT when it starts, and every write passes
//! the partition interlock first: only inside Freally OS's own partitions.
//! It serves the block protocol (TASK-100) to `ffs` alone: 4 KiB blocks
//! within a partition named by its GPT name.
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
use fcore_abi::{FErr, cache, perm, signal, syscall as nr, well_known};
use fstd::handle::Handle;
use fstd::ipc::{Channel, Client};
use fstd::time::{Duration, Instant};
use fwire::proto::block as blk;
use fwire::proto::conductor as c;
use fwire::proto::devmgr as d;

/// The completion in slot `k` of the queue at `va`, read so one half written
/// is never taken: its last dword (the command id and the phase bit
/// together) in one read first, then the other three (x86 keeps loads in
/// order). A plain 16-byte copy could see the new phase with the slot's old
/// command id: the ring then stepped past the completion it was waiting for
/// and waited for it in vain (TASK-185: seen in the soak, the CDP leg and
/// the update specs, the waited-for id found in the slot behind the head).
fn completion_at(va: u64, k: u16) -> Completion {
    let base = va + k as u64 * 16;
    // SAFETY: slot k is inside the completion queue's page, its dwords
    // aligned; the controller writes them.
    let read = |o: u64| unsafe { core::ptr::read_volatile((base + o) as *const u32) };
    let dw3 = read(12);
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::Acquire);
    let mut raw = [0u8; 16];
    for (i, d) in [read(0), read(4), read(8), dw3].into_iter().enumerate() {
        raw[4 * i..4 * i + 4].copy_from_slice(&d.to_le_bytes());
    }
    Completion::parse(&raw)
}

/// How long an I/O command may take (Linux's `io_timeout`): a VM's disk can
/// stall for seconds behind its host's writes (the soak and the CDP leg saw
/// 5 s pass, TASK-185), and a read given up loses the copy in hand.
const IO_WAIT: u64 = 30_000_000_000;

fstd::program!(run);

/// Entries in each queue.
const ENTRIES: u16 = 32;
/// Bring-ups tried before the driver gives up (TASK-092A).
const BRING_UP_TRIES: u32 = 2;
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

/// Commands whose completion woke the driver by interrupt (TASK-092A).
static WOKEN: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

struct Nvme {
    /// MSI-X's line (TASK-092A): completions wake the driver; `None`, polled.
    irq: Option<Handle>,
    regs: u64,
    /// The function's PCI configuration page.
    config: u64,
    cap: u64,
    admin: Queue,
    io: Option<Queue>,
    buf: Dma,
    list: Dma,
    block: u32,
    blocks: u64,
    interlock: Interlock,
    /// The partitions: name, first and last LBA, whether it's Freally OS's.
    parts: Vec<(String, u64, u64, bool)>,
    /// The controller's model and serial.
    model: String,
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
    /// The PCI command register (memory space bit 1, bus mastering bit 2).
    fn pci_command(&self) -> u16 {
        // SAFETY: the function's configuration page, mapped.
        unsafe { fstd::mmio::read16(self.config + 4) }
    }

    fn r32(&self, off: usize) -> u32 {
        // SAFETY: inside the mapped registers.
        unsafe { fstd::mmio::read32(self.regs + off as u64) }
    }

    fn w32(&self, off: usize, v: u32) {
        // SAFETY: inside the mapped registers.
        unsafe { fstd::mmio::write32(self.regs + off as u64, v) }
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

    /// The bring-up (§ 7.6.1), once: memory space and bus mastering on, the
    /// controller disabled, the admin queues given (the completion queue's
    /// old phase bits cleared), enabled, and the controller identified into
    /// `page` within 2 s (a controller that answers at all answers in
    /// microseconds).
    fn bring_up(&mut self, entries: u16, page: u64) -> Result<(), String> {
        // Memory space and bus mastering on (the command register, offset 4).
        // SAFETY: the function's configuration page, mapped.
        unsafe {
            let cmd = self.config + 4;
            fstd::mmio::write16(cmd, fstd::mmio::read16(cmd) | 0b110);
        }
        if self.r32(reg::CC) & 1 != 0 {
            self.w32(reg::CC, 0);
        }
        self.wait_ready(false)?;
        self.admin.ring = Ring::new(entries);
        // SAFETY: the admin completion queue's page, ours.
        unsafe { core::ptr::write_bytes(self.admin.cq.va as *mut u8, 0, 4096) };
        let size = (entries - 1) as u32;
        self.w32(reg::AQA, size << 16 | size);
        self.w64(reg::ASQ, self.admin.sq.phys);
        self.w64(reg::ACQ, self.admin.cq.phys);
        self.w32(reg::CC, drv_nvme::CC_ENABLE);
        self.wait_ready(true)?;
        self.run_within(false, Command::identify(1, 0, page), 2_000_000_000).map(|_| ())
    }

    /// Submits `cmd` on the admin queue (5 s) or the I/O queue (`IO_WAIT`)
    /// and waits for its completion.
    fn run(&mut self, io: bool, cmd: Command) -> Result<Completion, String> {
        let start = Instant::now();
        let done = self.run_within(io, cmd, if io { IO_WAIT } else { 5_000_000_000 });
        // A slow disk is said, not failed (a VM's behind its host's writes).
        let took = start.elapsed();
        if done.is_ok() && took > Duration::from_secs(5) {
            fstd::warn!("drv-nvme: command {:#x} took {:.1} s", cmd.opcode, took.as_secs_f64());
        }
        done
    }

    /// As [`run`], giving up after `timeout` ns.
    fn run_within(&mut self, io: bool, mut cmd: Command, timeout: u64) -> Result<Completion, String> {
        let (regs, cap, config) = (self.regs, self.cap, self.config);
        let q = if io { self.io.as_mut().ok_or("no I/O queue")? } else { &mut self.admin };
        let (slot, cid) = q.ring.submit();
        cmd.cid = cid;
        let bytes = cmd.bytes();
        // SAFETY: slot < ENTRIES, inside the submission queue's page.
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), (q.sq.va + slot as u64 * 64) as *mut u8, 64) };
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        // SAFETY: the doorbell is inside the mapped registers.
        unsafe { fstd::mmio::write32(regs + sq_doorbell(cap, q.qid) as u64, q.ring.sq_tail as u32) };
        let until = Instant::now().as_ns() + timeout;
        // Only I/O completions interrupt (MSI-X entry 1); the admin queue's
        // entry 0 stays masked, so it's polled.
        let irq = if io { self.irq.as_ref().map(Handle::raw) } else { None };
        let mut spins = 0u32;
        loop {
            let c = completion_at(q.cq.va, q.ring.cq_head);
            if let Some(head) = q.ring.consume(&c) {
                // SAFETY: as above.
                unsafe { fstd::mmio::write32(regs + cq_doorbell(cap, q.qid) as u64, head as u32) };
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
                // SAFETY: inside the mapped registers and configuration page.
                let (csts, pci) =
                    unsafe { (fstd::mmio::read32(regs + reg::CSTS as u64), fstd::mmio::read16(config + 4)) };
                // What the queues hold, for the next time it happens (seen after an
                // install given up, TASK-185): each completion slot's command id and
                // phase, and where the ring expects the next one.
                let slots: alloc::vec::Vec<String> = (0..q.ring.entries as u64)
                    .map(|k| {
                        let c = completion_at(q.cq.va, k as u16);
                        alloc::format!("{}{}", c.cid, if c.phase { "+" } else { "-" })
                    })
                    .collect();
                fstd::warn!(
                    "drv-nvme: queue {} at the timeout: cid {cid}, sq_tail {}, cq_head {}, phase {}; completions {}",
                    q.qid,
                    q.ring.sq_tail,
                    q.ring.cq_head,
                    q.ring.phase,
                    slots.join(" ")
                );
                return Err(alloc::format!(
                    "command {:#x} never completed (CSTS {csts:#x}, PCI command {pci:#x})",
                    cmd.opcode
                ));
            }
            match irq {
                // The completion's interrupt wakes it (and, should one go missing, 10 ms does).
                Some(h) => {
                    let wake = (Instant::now().as_ns() + 10_000_000).min(until);
                    if fstd::sys::wait_one(h, signal::SIGNALED, wake).is_ok() {
                        WOKEN.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                    }
                    let _ = fstd::sys::call1(nr::INTERRUPT_ACK, [h as u64, 0, 0, 0, 0, 0]);
                }
                None => {
                    spins += 1;
                    if spins.is_multiple_of(256) {
                        fstd::time::sleep(Duration::from_millis(1));
                    } else {
                        core::hint::spin_loop();
                    }
                }
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

    /// Writes `data` (whole blocks) at `lba`: the interlock first, then the
    /// controller. A refusal never reaches the device.
    fn write(&mut self, lba: u64, data: &[u8]) -> Result<(), FErr> {
        let count = data.len() as u64 / self.block as u64;
        self.interlock.check_write(lba, count)?;
        self.write_through(lba, data)
    }

    /// The table's own blocks (TASK-114's grow): the primary table (blocks 1
    /// to 33 of a 512-byte disk) or the backup at the end, and nothing else.
    fn write_table(&mut self, lba: u64, data: &[u8]) -> Result<(), FErr> {
        let count = data.len() as u64 / self.block as u64;
        let table = (16 * 1024 / self.block as u64) + 1;
        let primary = lba >= 1 && lba + count <= 1 + table;
        let backup = lba + table >= self.blocks && lba + count <= self.blocks;
        if !(primary || backup) {
            return Err(FErr::AccessDenied);
        }
        self.write_through(lba, data)
    }

    fn write_through(&mut self, lba: u64, data: &[u8]) -> Result<(), FErr> {
        let count = data.len() as u64 / self.block as u64;
        if data.is_empty() || !(data.len() as u64).is_multiple_of(self.block as u64) || data.len() as u64 > BUF {
            return Err(FErr::InvalidArgs);
        }
        // SAFETY: `data` fits the buffer.
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), self.buf.va as *mut u8, data.len()) };
        let prp = self.prp(data.len() as u64).map_err(|_| FErr::InvalidArgs)?;
        self.run(true, Command::rw(true, 1, lba, count as u32, prp)).map(|_| ()).map_err(|e| {
            fstd::error!("drv-nvme: {e}");
            FErr::Internal
        })
    }

    fn flush(&mut self) -> Result<(), String> {
        self.run(true, Command::flush(1)).map(|_| ())
    }
}

/// MSI-X on (TASK-092A): the capability found in the configuration page,
/// entry 1 of its table given the kernel's vector (the I/O queue's), legacy
/// INTx off. Entry 0, the admin queue's, stays masked: QEMU keeps the table
/// across a reset, and its firmware, which always uses the admin queue,
/// missed the disk on a third of warm resets while entry 0 still rang (none
/// with it masked: crash-test, 12 resets each). Its line and vector; `None`
/// (the driver polls, as before) if something's missing.
fn msix(registers: &Handle, table_page: Option<&Handle>, cap: u64, bar0: u64, config: u64) -> Option<(Handle, u32)> {
    // SAFETY: offsets inside the function's 4 KiB configuration page, mapped.
    let rd = |o: u64, n: u32| unsafe {
        match n {
            1 => fstd::mmio::read8(config + o) as u32,
            2 => fstd::mmio::read16(config + o) as u32,
            _ => fstd::mmio::read32(config + o),
        }
    };
    // SAFETY: as above.
    let wr16 = |o: u64, v: u16| unsafe { fstd::mmio::write16(config + o, v) };
    // devmgr found the capability (and granted the table's page).
    let at = cap;
    if rd(at, 1) != 0x11 {
        fstd::warn!("drv-nvme: no MSI-X capability at {at:#x}: completions are polled");
        return None;
    }
    let control = rd(at + 2, 2) as u16;
    if control & 0x7FF < 1 {
        fstd::info!("drv-nvme: the MSI-X table has one entry: completions are polled");
        return None;
    }
    let table = rd(at + 4, 4);
    if table & 7 != 0 {
        fstd::info!("drv-nvme: the MSI-X table isn't in BAR 0: completions are polled");
        return None;
    }
    let offset = (table & !7) as u64;
    let page = offset & !0xFFF;
    // The table's page: inside the registers' grant, or its own.
    let grant = if page < 0x2000 { Some(registers) } else { table_page };
    let Some(grant) = grant else {
        fstd::info!("drv-nvme: devmgr granted no MSI-X table: completions are polled");
        return None;
    };
    let mapped = match map_mmio(grant, bar0 + page, 4096) {
        Ok(m) => m,
        Err(e) => {
            fstd::warn!("drv-nvme: the MSI-X table: {e}: completions are polled");
            return None;
        }
    };
    let entry = mapped + (offset - page);
    let (h, message) = match fstd::sys::call(nr::INTERRUPT_BIND_MSI, [registers.raw() as u64, 0, 0, 0, 0, 0]) {
        Ok(r) => r,
        Err(e) => {
            fstd::warn!("drv-nvme: no MSI-X vector ({}): completions are polled", e.describe());
            return None;
        }
    };
    let Some(line) = Handle::from_raw(h as u32) else {
        fstd::warn!("drv-nvme: the kernel gave MSI-X no handle: completions are polled");
        return None;
    };
    let (address, data) = (message as u32, (message >> 32) as u32);
    // SAFETY: entries 0 and 1 of the table (16 bytes each), mapped above:
    // entry 0 masked (its vector control's bit 0), entry 1 ours.
    unsafe {
        fstd::mmio::write32(entry + 12, 1);
        let one = entry + 16;
        fstd::mmio::write32(one, address);
        fstd::mmio::write32(one + 4, 0);
        fstd::mmio::write32(one + 8, data);
        fstd::mmio::write32(one + 12, 0);
    }
    // Enabled, the function unmasked; INTx off (command bit 10).
    wr16(at + 2, (control | 1 << 15) & !(1 << 14));
    wr16(4, rd(4, 2) as u16 | 1 << 10);
    Some((line, data))
}

fn open() -> Result<Nvme, String> {
    let devmgr = fstd::service::connect("devmgr").map_err(|e| alloc::format!("devmgr: {e:?}"))?;
    let mut client = Client::new(devmgr);
    let (r, handles) = client
        .call::<_, d::Resources>(d::RESOURCES, &(), Vec::new(), Instant::after(Duration::from_secs(2)))
        .map_err(|e| match e {
            // devmgr found no controller for this driver.
            fstd::ipc::CallError::Remote(code, _) if code == FErr::NotFound as i32 => String::from(NO_CONTROLLER),
            e => alloc::format!("devmgr's resources: {e:?}"),
        })?;
    let take = |name: &str| r.handles.iter().find(|(n, _)| n == name).map(|(_, h)| h.0 as usize);
    let (Some(ri), Some(ci)) = (take("registers"), take("config")) else {
        return Err(String::from(NO_CONTROLLER));
    };
    let regs = map_mmio(&handles[ri], value(&r, "bar0"), 0x2000)?;
    let config = map_mmio(&handles[ci], value(&r, "config"), 4096)?;
    // SAFETY: inside the mapped registers.
    let cap = unsafe { fstd::mmio::read64(regs) };
    let entries = ENTRIES.min(drv_nvme::max_entries(cap).min(u16::MAX as u32) as u16);
    let queue = |qid: u16| -> Result<Queue, String> {
        Ok(Queue { qid, sq: Dma::new(4096)?, cq: Dma::new(4096)?, ring: Ring::new(entries) })
    };
    let mut n = Nvme {
        irq: None,
        regs,
        config,
        cap,
        admin: queue(0)?,
        io: None,
        buf: Dma::new(BUF)?,
        list: Dma::new(4096)?,
        block: 512,
        blocks: 0,
        interlock: Interlock::read_only(),
        parts: Vec::new(),
        model: String::new(),
    };
    fstd::info!(
        "drv-nvme: as the firmware left it: CC {:#x}, CSTS {:#x}, PCI command {:#x}",
        n.r32(reg::CC),
        n.r32(reg::CSTS),
        n.pci_command()
    );
    let page = Dma::new(4096)?;
    // A controller can be deaf to its first command (TASK-092A): reset once
    // and ask again. (On QEMU's very first start with fresh firmware
    // variables it can stay deaf through any reset, a function reset
    // included: only a fresh QEMU process helps. The tester guide says so.)
    let mut tries = 1;
    while let Err(e) = n.bring_up(entries, page.phys) {
        if tries == BRING_UP_TRIES {
            return Err(e);
        }
        fstd::warn!("drv-nvme: {e}: resetting the controller and asking again");
        tries += 1;
    }
    if tries > 1 {
        fstd::info!("drv-nvme: the controller answered after a reset");
    }
    // MSI-X once the controller answers (the admin queue is polled).
    let irq = match r.values.iter().find(|(n, _)| n == "msix-cap") {
        Some(&(_, cap)) => msix(&handles[ri], take("msix-table").map(|i| &handles[i]), cap, value(&r, "bar0"), config),
        None => {
            fstd::info!("drv-nvme: no MSI-X capability: completions are polled");
            None
        }
    };
    match &irq {
        Some((_, vector)) => fstd::info!("drv-nvme: MSI-X on: completions interrupt on vector {vector}"),
        None => fstd::info!("drv-nvme: polling completions"),
    }
    n.irq = irq.map(|(h, _)| h);
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
    let cq = if n.irq.is_some() {
        Command::create_cq_irq(1, entries as u32, io.cq.phys, 1)
    } else {
        Command::create_cq(1, entries as u32, io.cq.phys)
    };
    n.run(false, cq)?;
    n.run(false, Command::create_sq(1, entries as u32, io.sq.phys))?;
    n.io = Some(io);
    fstd::info!(
        "drv-nvme: {model} (serial {serial}, firmware {fw}): {blocks} blocks of {block} bytes, {entries}-entry queues"
    );
    n.model = alloc::format!("{model} {serial}");
    grow_gpt(&mut n);
    (n.interlock, n.parts) = read_gpt(&mut n);
    if n.irq.is_some() {
        fstd::info!(
            "drv-nvme: {} command(s) so far woken by their interrupt",
            WOKEN.load(core::sync::atomic::Ordering::Relaxed)
        );
    }
    Ok(n)
}

/// The disk's GPT (the primary, else the backup) as the interlock, and its
/// partitions; a disk without one is read-only.
/// A disk grown since its table was written (a tester's virtual disk made
/// bigger before its first boot, TASK-114): FREALLY_DATA, the last
/// partition, takes the new room (`fgpt::grow`, only on a table of Freally
/// OS's layout). The one place drv-nvme writes a table: the new backup first
/// (where nothing was), then the primary's entries and header, so a cut
/// leaves the old table whole.
fn grow_gpt(n: &mut Nvme) {
    let bs = n.block;
    let last = n.blocks - 1;
    let Ok(head) = n.read(1, 1).map(|b| b.to_vec()) else { return };
    let Ok(h) = fgpt::Header::parse(&head, 1, last) else { return };
    let count = (h.entries_len() as u64).div_ceil(bs as u64) as u32;
    let Ok(mut entries) = n.read(h.entries_lba, count).map(|b| b.to_vec()) else { return };
    let Some(g) = fgpt::grow(&head, &mut entries, n.blocks, bs) else { return };
    let block = |hd: &[u8; fgpt::HEADER_SIZE as usize]| {
        let mut b = alloc::vec![0u8; bs as usize];
        b[..hd.len()].copy_from_slice(hd);
        b
    };
    let steps = [
        (g.backup_entries_lba, entries.clone()),
        (g.backup_lba, block(&g.backup)),
        (h.entries_lba, entries),
        (1, block(&g.primary)),
    ];
    for (lba, bytes) in steps {
        if let Err(e) = n.write_table(lba, &bytes) {
            fstd::error!("drv-nvme: growing the table at block {lba}: {e:?}");
            return;
        }
    }
    match n.flush() {
        Ok(()) => fstd::info!(
            "drv-nvme: the disk grew: FREALLY_DATA now ends at block {} ({} MiB of disk)",
            g.data_last,
            (n.blocks * bs as u64) >> 20
        ),
        Err(e) => fstd::error!("drv-nvme: the grown table's flush: {e}"),
    }
}

fn read_gpt(n: &mut Nvme) -> (Interlock, Vec<(String, u64, u64, bool)>) {
    let bs = n.block as u64;
    let last = n.blocks - 1;
    for lba in [1, last] {
        let Ok(head) = n.read(lba, 1).map(|b| b.to_vec()) else { continue };
        let Ok(h) = fgpt::Header::parse(&head, lba, last) else { continue };
        let count = (h.entries_len() as u64).div_ceil(bs) as u32;
        let Ok(entries) = n.read(h.entries_lba, count).map(|b| b.to_vec()) else { continue };
        let Ok(t) = fgpt::Table::new(h, &entries) else { continue };
        let mut parts = Vec::new();
        for p in t.partitions() {
            let len = p.name.iter().position(|&u| u == 0).unwrap_or(p.name.len());
            let name = String::from_utf16_lossy(&p.name[..len]);
            let freally = p.type_guid == fgpt::PART_TYPE_FREALLY_SYSTEM || p.type_guid == fgpt::PART_TYPE_FREALLY_DATA;
            parts.push((name.clone(), p.first_lba, p.last_lba, freally));
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
        return (i, parts);
    }
    fstd::info!("drv-nvme: no GPT: the disk is read-only");
    (Interlock::read_only(), Vec::new())
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
    let Some(lba) = n.parts.iter().find(|p| p.0 == "FREALLY_DATA").map(|p| p.2) else {
        fstd::info!("drv-nvme: e2e: no FREALLY_DATA to write in");
        return;
    };
    // FreallyFS owns the partition: the block is put back as it was.
    let Ok(before) = n.read(lba, 1).map(|b| b.to_vec()) else { return };
    let pattern: Vec<u8> = (0..n.block).map(|i| (i * 7 + 3) as u8).collect();
    let ok =
        n.write(lba, &pattern).is_ok() && n.flush().is_ok() && n.read(lba, 1).is_ok_and(|b| b == pattern.as_slice());
    let restored = n.write(lba, &before).is_ok() && n.flush().is_ok();
    if ok && restored {
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
        // A machine with no NVMe drive (a VM on the files layout): nothing
        // to drive, and nothing wrong.
        Err(e) if e == NO_CONTROLLER => {
            fstd::info!("drv-nvme: {e}");
            return 0;
        }
        Err(e) => {
            fstd::error!("drv-nvme: {e}");
            return 1;
        }
    };
    hold(n)
}

/// What `open` says when devmgr found no controller.
const NO_CONTROLLER: &str = "devmgr found no NVMe controller";

/// Serves the block protocol (the e2e image proves the driver first).
fn hold(n: Nvme) -> i64 {
    #[cfg(feature = "flavour-e2e")]
    let n = {
        let mut n = n;
        prove(&mut n);
        n
    };
    let mut n = n;
    let Ok((listen, give)) = Channel::pair() else { return 1 };
    if fstd::service::register(give).is_err() {
        return 1;
    }
    let mut clients: Vec<(String, Channel)> = Vec::new();
    loop {
        let mut items = alloc::vec![[listen.0.raw(), signal::READABLE]];
        items.extend(clients.iter().map(|(_, ch)| [ch.0.raw(), signal::READABLE | signal::PEER_CLOSED]));
        match fstd::sys::wait_many(&items, u64::MAX) {
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
                let (who, ch) = &clients[k - 1];
                if !serve(&mut n, who, ch) {
                    clients.remove(k - 1);
                }
            }
            Err(_) => fstd::time::sleep(Duration::from_millis(10)),
        }
    }
}

/// A partition's first LBA and LBA count for `count` 4 KiB blocks from
/// `block`, wholly inside it.
fn lbas(n: &Nvme, part: &str, block: u64, count: u32) -> Result<(u64, u32), FErr> {
    let per = 4096 / n.block as u64;
    let (_, first, last, _) = n.parts.iter().find(|p| p.0 == part).ok_or(FErr::NotFound)?;
    let blocks = (last - first + 1) / per;
    let end = block.checked_add(count as u64).ok_or(FErr::InvalidArgs)?;
    if count == 0 || count > blk::MAX_BLOCKS || end > blocks {
        return Err(FErr::InvalidArgs);
    }
    Ok((first + block * per, (count as u64 * per) as u32))
}

/// One client's requests until its channel is empty; false when it's gone.
/// Only ffs and the updater may use the disk, each its own partitions.
fn serve(n: &mut Nvme, who: &str, ch: &Channel) -> bool {
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
        let err = |e: FErr, why: &str| fwire::error_reply(h.txid, h.ordinal, e as i32, why);
        let mine = |part: &str| drv_nvme::may_use(who, part);
        let reply = if !matches!(who, "ffs" | "updater") {
            err(FErr::AccessDenied, "only ffs and the updater use the disk")
        } else {
            match h.ordinal {
                blk::INFO => {
                    let per = 4096 / n.block as u64;
                    let partitions = (n.parts.iter())
                        .map(|(name, f, l, fr)| blk::Partition {
                            name: name.clone(),
                            blocks: (l - f + 1) / per,
                            freally: *fr,
                        })
                        .collect();
                    fwire::reply(h.txid, h.ordinal, &blk::Info { model: n.model.clone(), partitions })
                }
                blk::READ => match fwire::from_payload::<blk::Read>(p) {
                    Ok(r) if !mine(&r.partition) => err(FErr::AccessDenied, "not a partition of yours"),
                    Ok(r) => match lbas(n, &r.partition, r.block, r.count) {
                        Ok((lba, count)) => match n.read(lba, count) {
                            Ok(bytes) => fwire::reply(h.txid, h.ordinal, &blk::Data { bytes: bytes.to_vec() }),
                            Err(e) => {
                                fstd::error!("drv-nvme: {e}");
                                err(FErr::Internal, "the read failed")
                            }
                        },
                        Err(e) => err(e, "outside the partition"),
                    },
                    Err(e) => err(FErr::InvalidArgs, e.describe()),
                },
                blk::WRITE => match fwire::from_payload::<blk::Write>(p) {
                    Ok(w) if !mine(&w.partition) => err(FErr::AccessDenied, "not a partition of yours"),
                    Ok(w) if w.bytes.len().is_multiple_of(4096) => {
                        match lbas(n, &w.partition, w.block, (w.bytes.len() / 4096) as u32) {
                            // The interlock stands in `write` itself, before the controller.
                            Ok((lba, _)) => match n.write(lba, &w.bytes) {
                                Ok(()) => fwire::reply(h.txid, h.ordinal, &()),
                                Err(e) => err(e, "the write was refused"),
                            },
                            Err(e) => err(e, "outside the partition"),
                        }
                    }
                    Ok(_) => err(FErr::InvalidArgs, "whole 4 KiB blocks only"),
                    Err(e) => err(FErr::InvalidArgs, e.describe()),
                },
                blk::FLUSH => match n.flush() {
                    Ok(()) => fwire::reply(h.txid, h.ordinal, &()),
                    Err(e) => {
                        fstd::error!("drv-nvme: {e}");
                        err(FErr::Internal, "the flush failed")
                    }
                },
                _ => err(FErr::NotSupported, "not served"),
            }
        };
        if let Ok(msg) = reply {
            let _ = ch.send(&msg, Vec::new());
        }
    }
}
