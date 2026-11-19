// SPDX-License-Identifier: Apache-2.0
//! drv-nvme (TASK-092): NVM Express, written from the NVMe 1.4 base
//! specification. This library is the driver's logic, tested on the host:
//! the controller's registers, the 64-byte commands, the 16-byte
//! completions, the queues' rings and phases, the PRP lists, and the
//! partition interlock ([`interlock`]). `main.rs` puts it on a real
//! controller: devmgr's resources, DMA memory, the doorbells.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod interlock;

/// The controller's registers (§ 3.1), as offsets into BAR0.
pub mod reg {
    /// Capabilities: MQES (bits 0–15, entries − 1), TO (24–31, 500 ms
    /// units), DSTRD (32–35), CSS (37–44), MPSMIN (48–51).
    pub const CAP: usize = 0x00;
    pub const VS: usize = 0x08;
    /// Controller configuration: EN (0), CSS (4–6), MPS (7–10), IOSQES
    /// (16–19), IOCQES (20–23).
    pub const CC: usize = 0x14;
    /// Controller status: RDY (0), CFS (1).
    pub const CSTS: usize = 0x1C;
    /// Admin queue attributes: ASQS (0–11), ACQS (16–27), each entries − 1.
    pub const AQA: usize = 0x24;
    pub const ASQ: usize = 0x28;
    pub const ACQ: usize = 0x30;
    /// The first doorbell.
    pub const DOORBELLS: usize = 0x1000;
}

/// CC for an enabled controller with 4 KiB pages, 64-byte submission and
/// 16-byte completion entries, the NVM command set.
/// (CSS 0 and MPS 0 are the zeros between.)
pub const CC_ENABLE: u32 = 1 | (6 << 16) | (4 << 20);

/// The submission doorbell of queue `qid` (§ 3.1.24), with the doorbell stride from CAP.
pub fn sq_doorbell(cap: u64, qid: u16) -> usize {
    reg::DOORBELLS + (2 * qid as usize) * (4 << ((cap >> 32) & 0xF))
}

/// Its completion doorbell.
pub fn cq_doorbell(cap: u64, qid: u16) -> usize {
    reg::DOORBELLS + (2 * qid as usize + 1) * (4 << ((cap >> 32) & 0xF))
}

/// The most entries a queue may have (CAP.MQES + 1).
pub fn max_entries(cap: u64) -> u32 {
    (cap & 0xFFFF) as u32 + 1
}

/// How long the controller may take to become ready (CAP.TO), in ms.
pub fn ready_timeout_ms(cap: u64) -> u64 {
    ((cap >> 24) & 0xFF).max(1) * 500
}

/// Opcodes: admin (§ 5) and NVM (NVM command set § 3).
pub mod op {
    pub const DELETE_IO_SQ: u8 = 0x00;
    pub const CREATE_IO_SQ: u8 = 0x01;
    pub const DELETE_IO_CQ: u8 = 0x04;
    pub const CREATE_IO_CQ: u8 = 0x05;
    pub const IDENTIFY: u8 = 0x06;
    pub const FLUSH: u8 = 0x00;
    pub const WRITE: u8 = 0x01;
    pub const READ: u8 = 0x02;
}

/// A 64-byte submission queue entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Command {
    pub opcode: u8,
    pub cid: u16,
    pub nsid: u32,
    pub prp1: u64,
    pub prp2: u64,
    /// CDW10 to CDW15.
    pub cdw: [u32; 6],
}

impl Command {
    pub fn bytes(&self) -> [u8; 64] {
        let mut b = [0u8; 64];
        b[0] = self.opcode;
        b[2..4].copy_from_slice(&self.cid.to_le_bytes());
        b[4..8].copy_from_slice(&self.nsid.to_le_bytes());
        b[24..32].copy_from_slice(&self.prp1.to_le_bytes());
        b[32..40].copy_from_slice(&self.prp2.to_le_bytes());
        for (i, d) in self.cdw.iter().enumerate() {
            b[40 + 4 * i..44 + 4 * i].copy_from_slice(&d.to_le_bytes());
        }
        b
    }

    /// Identify (§ 5.15): CNS 1 the controller, CNS 0 namespace `nsid`; 4 KiB to `page`.
    pub fn identify(cns: u32, nsid: u32, page: u64) -> Command {
        Command { opcode: op::IDENTIFY, nsid, prp1: page, cdw: [cns, 0, 0, 0, 0, 0], ..Default::default() }
    }

    /// Create I/O Completion Queue `qid` of `entries` at `page` (physically
    /// contiguous), interrupts off: the driver polls it.
    pub fn create_cq(qid: u16, entries: u32, page: u64) -> Command {
        let size = (entries - 1) << 16 | qid as u32;
        Command { opcode: op::CREATE_IO_CQ, prp1: page, cdw: [size, 1, 0, 0, 0, 0], ..Default::default() }
    }

    /// Create I/O Submission Queue `qid` of `entries` at `page`, completing into CQ `qid`.
    pub fn create_sq(qid: u16, entries: u32, page: u64) -> Command {
        let size = (entries - 1) << 16 | qid as u32;
        Command {
            opcode: op::CREATE_IO_SQ,
            prp1: page,
            cdw: [size, (qid as u32) << 16 | 1, 0, 0, 0, 0],
            ..Default::default()
        }
    }

    /// Read or Write `blocks` (1–65536) from `lba` of namespace `nsid`, the data at `prp`.
    pub fn rw(write: bool, nsid: u32, lba: u64, blocks: u32, prp: (u64, u64)) -> Command {
        Command {
            opcode: if write { op::WRITE } else { op::READ },
            nsid,
            prp1: prp.0,
            prp2: prp.1,
            cdw: [lba as u32, (lba >> 32) as u32, blocks - 1, 0, 0, 0],
            ..Default::default()
        }
    }

    pub fn flush(nsid: u32) -> Command {
        Command { opcode: op::FLUSH, nsid, ..Default::default() }
    }
}

/// A 16-byte completion queue entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Completion {
    pub dw0: u32,
    pub sq_head: u16,
    pub sq_id: u16,
    pub cid: u16,
    pub phase: bool,
    /// Status code type (bits 9–11 of the status field) and code (1–8): 0 is success.
    pub status: u16,
}

impl Completion {
    pub fn parse(b: &[u8; 16]) -> Completion {
        let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
        let sf = u16_at(14);
        Completion {
            dw0: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            sq_head: u16_at(8),
            sq_id: u16_at(10),
            cid: u16_at(12),
            phase: sf & 1 != 0,
            status: (sf >> 1) & 0x7FF,
        }
    }

    pub fn ok(&self) -> bool {
        self.status == 0
    }
}

/// A queue pair's bookkeeping: where the next command goes, where the next
/// completion is, and the phase that marks a new one (§ 4.6).
#[derive(Clone, Copy, Debug)]
pub struct Ring {
    pub entries: u16,
    pub sq_tail: u16,
    pub cq_head: u16,
    pub phase: bool,
    next_cid: u16,
}

impl Ring {
    pub fn new(entries: u16) -> Ring {
        Ring { entries, sq_tail: 0, cq_head: 0, phase: true, next_cid: 0 }
    }

    /// Takes the next submission slot and command id: (slot, cid); the new
    /// tail is the doorbell's value.
    pub fn submit(&mut self) -> (u16, u16) {
        let slot = self.sq_tail;
        self.sq_tail = (self.sq_tail + 1) % self.entries;
        self.next_cid = self.next_cid.wrapping_add(1);
        (slot, self.next_cid)
    }

    /// Whether the entry at the head is new (its phase is ours); if so,
    /// moves past it (wrapping flips the phase) and returns the new head,
    /// the completion doorbell's value.
    pub fn consume(&mut self, c: &Completion) -> Option<u16> {
        if c.phase != self.phase {
            return None;
        }
        self.cq_head += 1;
        if self.cq_head == self.entries {
            self.cq_head = 0;
            self.phase = !self.phase;
        }
        Some(self.cq_head)
    }
}

/// The PRP entries (§ 4.3) for a transfer of `len` bytes at the physical
/// pages `pages` (4 KiB each, the first used from `offset`): PRP1, and PRP2
/// as the second page, or as `list` (a page of the rest's addresses, at
/// physical `list_phys`), or 0. `None` if the transfer needs more than one
/// list page (512 entries: 2 MiB) or more pages than given.
pub fn prps(pages: &[u64], offset: u64, len: u64, list: &mut [u64; 512], list_phys: u64) -> Option<(u64, u64)> {
    if len == 0 || offset >= 4096 {
        return None;
    }
    let n = (offset + len).div_ceil(4096) as usize;
    if n > pages.len() {
        return None;
    }
    let prp1 = pages[0] + offset;
    Some(match n {
        1 => (prp1, 0),
        2 => (prp1, pages[1]),
        _ if n - 1 <= list.len() => {
            list[..n - 1].copy_from_slice(&pages[1..n]);
            (prp1, list_phys)
        }
        _ => return None,
    })
}

/// What Identify Controller says: its model, serial and firmware, trimmed.
pub fn identify_controller(page: &[u8]) -> (&str, &str, &str) {
    let text = |a: usize, b: usize| core::str::from_utf8(&page[a..b]).unwrap_or("").trim();
    (text(24, 64), text(4, 24), text(64, 72))
}

/// What Identify Namespace says: its size in blocks and the block size
/// (from the formatted LBA format), or `None` for an inactive namespace.
pub fn identify_namespace(page: &[u8]) -> Option<(u64, u32)> {
    let size = u64::from_le_bytes(page[0..8].try_into().ok()?);
    let flbas = (page[26] & 0xF) as usize;
    let lbaf = u32::from_le_bytes(page[128 + 4 * flbas..132 + 4 * flbas].try_into().ok()?);
    let shift = (lbaf >> 16) & 0xFF;
    (size > 0 && (9..=12).contains(&shift)).then_some((size, 1 << shift))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_command_is_laid_out_as_the_spec_says() {
        let c = Command { cid: 7, ..Command::rw(false, 1, 0x1_2345_6789, 8, (0x1000, 0x2000)) };
        let b = c.bytes();
        assert_eq!((b[0], u16::from_le_bytes([b[2], b[3]])), (op::READ, 7));
        assert_eq!(u32::from_le_bytes(b[4..8].try_into().unwrap()), 1, "namespace 1");
        assert_eq!(u64::from_le_bytes(b[24..32].try_into().unwrap()), 0x1000);
        assert_eq!(u64::from_le_bytes(b[32..40].try_into().unwrap()), 0x2000);
        assert_eq!(u32::from_le_bytes(b[40..44].try_into().unwrap()), 0x2345_6789, "the LBA's low half");
        assert_eq!(u32::from_le_bytes(b[44..48].try_into().unwrap()), 1, "its high half");
        assert_eq!(u32::from_le_bytes(b[48..52].try_into().unwrap()), 7, "8 blocks, zero-based");
        let w = Command::rw(true, 1, 0, 1, (0, 0));
        assert_eq!(w.opcode, op::WRITE);
        let q = Command::create_sq(1, 64, 0x9000);
        assert_eq!((q.cdw[0], q.cdw[1]), (63 << 16 | 1, 1 << 16 | 1), "64 entries, CQ 1, contiguous");
    }

    #[test]
    fn completions_and_the_phase() {
        let mut raw = [0u8; 16];
        raw[12..14].copy_from_slice(&9u16.to_le_bytes());
        raw[14..16].copy_from_slice(&1u16.to_le_bytes());
        let c = Completion::parse(&raw);
        assert!(c.ok() && c.phase && c.cid == 9);
        raw[14..16].copy_from_slice(&(1u16 | 0x2 << 1).to_le_bytes());
        assert!(!Completion::parse(&raw).ok(), "status code 2");
        // A 2-entry ring: two completions with phase 1, then the phase flips.
        let mut r = Ring::new(2);
        let new = Completion { phase: true, ..c };
        assert_eq!(r.consume(&new), Some(1));
        assert_eq!(r.consume(&new), Some(0));
        assert_eq!(r.consume(&new), None, "phase 1 again is an old entry now");
        assert_eq!(r.consume(&Completion { phase: false, ..c }), Some(1));
        assert_eq!((r.submit(), r.submit(), r.submit()), ((0, 1), (1, 2), (0, 3)), "slots wrap, ids don't repeat");
    }

    #[test]
    fn prp_entries_by_length() {
        let pages = [0x10_000, 0x20_000, 0x30_000, 0x40_000];
        let mut list = [0u64; 512];
        assert_eq!(prps(&pages, 0, 4096, &mut list, 0x99_000), Some((0x10_000, 0)), "one page");
        assert_eq!(prps(&pages, 512, 4096, &mut list, 0x99_000), Some((0x10_200, 0x20_000)), "two pages");
        assert_eq!(prps(&pages, 0, 3 * 4096, &mut list, 0x99_000), Some((0x10_000, 0x99_000)), "a list");
        assert_eq!(&list[..2], &[0x20_000, 0x30_000]);
        assert_eq!(prps(&pages, 0, 5 * 4096, &mut list, 0x99_000), None, "more than the pages given");
        assert_eq!(prps(&pages, 0, 0, &mut list, 0x99_000), None);
        assert_eq!(prps(&pages, 4096, 8, &mut list, 0x99_000), None, "an offset past the page");
    }

    #[test]
    fn registers_and_identify() {
        let cap = 0x0000_0020_1400_07FFu64; // DSTRD 0, TO 20 (10 s), MQES 2047
        assert_eq!((sq_doorbell(cap, 0), cq_doorbell(cap, 0), sq_doorbell(cap, 1)), (0x1000, 0x1004, 0x1008));
        assert_eq!((max_entries(cap), ready_timeout_ms(cap)), (2048, 10_000));
        assert_eq!(sq_doorbell(cap | 1 << 32, 1), 0x1010, "a stride of 8");
        let mut page = [b' '; 4096];
        page[4..12].copy_from_slice(b"SERIAL01");
        page[24..34].copy_from_slice(b"QEMU NVMe ");
        page[64..67].copy_from_slice(b"1.0");
        assert_eq!(identify_controller(&page), ("QEMU NVMe", "SERIAL01", "1.0"));
        let mut ns = [0u8; 4096];
        ns[..8].copy_from_slice(&4096u64.to_le_bytes());
        ns[26] = 1;
        ns[132..136].copy_from_slice(&(12u32 << 16).to_le_bytes());
        assert_eq!(identify_namespace(&ns), Some((4096, 4096)), "LBA format 1: 4 KiB blocks");
        ns[132..136].copy_from_slice(&(20u32 << 16).to_le_bytes());
        assert_eq!(identify_namespace(&ns), None, "a block size the driver doesn't take");
    }
}
