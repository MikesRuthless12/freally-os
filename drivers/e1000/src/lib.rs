// SPDX-License-Identifier: Apache-2.0
//! drv-e1000 (TASK-173): Intel's 82540EM (e1000) and 82574L (e1000e) in
//! legacy descriptor mode, written from Intel's *PCI/PCI-X Family of Gigabit
//! Ethernet Controllers Software Developer's Manual* (8254x) and the 82574
//! datasheet. This library is the driver's logic, tested on the host: the
//! registers and their values, the 16-byte descriptors, both rings' index
//! arithmetic, and the MAC from RAL/RAH. `main.rs` puts it on a real card.
#![cfg_attr(not(test), no_std)]

/// The cards: 82540EM (QEMU's `e1000`, VirtualBox's PRO/1000 MT Desktop),
/// 82545EM (VMware's `e1000`), 82574L (QEMU's and VMware's `e1000e`).
pub fn is_e1000(vendor: u16, id: u16) -> bool {
    vendor == 0x8086 && matches!(id, 0x100E | 0x100F | 0x10D3)
}

/// Register offsets into BAR 0 (§ 13).
pub mod reg {
    pub const CTRL: u64 = 0x0000;
    pub const STATUS: u64 = 0x0008;
    pub const ICR: u64 = 0x00C0;
    pub const IMC: u64 = 0x00D8;
    pub const RCTL: u64 = 0x0100;
    pub const TCTL: u64 = 0x0400;
    pub const TIPG: u64 = 0x0410;
    pub const RDBAL: u64 = 0x2800;
    pub const RDBAH: u64 = 0x2804;
    pub const RDLEN: u64 = 0x2808;
    pub const RDH: u64 = 0x2810;
    pub const RDT: u64 = 0x2818;
    pub const TDBAL: u64 = 0x3800;
    pub const TDBAH: u64 = 0x3804;
    pub const TDLEN: u64 = 0x3808;
    pub const TDH: u64 = 0x3810;
    pub const TDT: u64 = 0x3818;
    /// The multicast table: 128 registers.
    pub const MTA: u64 = 0x5200;
    pub const RAL0: u64 = 0x5400;
    pub const RAH0: u64 = 0x5404;
    /// The register space BAR 0 maps: 128 KiB.
    pub const SIZE: u64 = 0x20000;
}

/// CTRL: set link up, auto-speed detection; the reset bit.
pub mod ctrl {
    pub const ASDE: u32 = 1 << 5;
    pub const SLU: u32 = 1 << 6;
    pub const RST: u32 = 1 << 26;
    /// Bits that hold the link down or reset the PHY: cleared.
    pub const HOLD: u32 = 1 << 3 | 1 << 7 | 1 << 30 | 1 << 31;
}

/// STATUS: the link is up.
pub const STATUS_LU: u32 = 1 << 1;

/// RCTL: on, broadcasts accepted, 2048-byte buffers (BSIZE 00), the CRC
/// stripped.
pub const RCTL: u32 = 1 << 1 | 1 << 15 | 1 << 26;
/// TCTL: on, short packets padded, collision threshold 0x0F, distance 0x40 (full duplex).
pub const TCTL: u32 = 1 << 1 | 1 << 3 | 0x0F << 4 | 0x40 << 12;
/// TIPG for copper (§ 13.4.34): IPGT 10, IPGR1 8, IPGR2 6.
pub const TIPG: u32 = 10 | 8 << 10 | 6 << 20;

/// A descriptor's status bits: done, end of packet.
pub const DD: u8 = 1;
pub const EOP: u8 = 2;
/// A transmit descriptor's command: end of packet, insert the FCS, report status.
pub const TX_CMD: u8 = 1 | 1 << 1 | 1 << 3;

/// Entries in each ring (a 2 KiB descriptor table, 8 × 16 bytes aligned).
pub const ENTRIES: u16 = 128;
/// Each buffer.
pub const BUF: u64 = 2048;

/// The MAC from RAL0/RAH0, if the address-valid bit says it's there.
pub fn mac_of(ral: u32, rah: u32) -> Option<[u8; 6]> {
    if rah & 1 << 31 == 0 {
        return None;
    }
    let l = ral.to_le_bytes();
    let h = rah.to_le_bytes();
    let mac = [l[0], l[1], l[2], l[3], h[0], h[1]];
    (mac != [0; 6] && mac[0] & 1 == 0).then_some(mac)
}

/// A receive descriptor's fields after the card wrote it (§ 3.2.3):
/// length at 8, status at 12, errors at 13.
pub fn rx_done(d: &[u8; 16]) -> Option<(usize, bool)> {
    let status = d[12];
    if status & DD == 0 {
        return None;
    }
    let len = u16::from_le_bytes([d[8], d[9]]) as usize;
    // Whole and error-free: one buffer holds every frame (no jumbo frames).
    Some((len, status & EOP != 0 && d[13] == 0))
}

/// A receive descriptor handed to the card: its buffer, its status cleared.
pub fn rx_desc(addr: u64) -> [u8; 16] {
    let mut d = [0u8; 16];
    d[..8].copy_from_slice(&addr.to_le_bytes());
    d
}

/// A transmit descriptor (legacy, § 3.3.3): the buffer, its length, the command.
pub fn tx_desc(addr: u64, len: u16) -> [u8; 16] {
    let mut d = [0u8; 16];
    d[..8].copy_from_slice(&addr.to_le_bytes());
    d[8..10].copy_from_slice(&len.to_le_bytes());
    d[11] = TX_CMD;
    d
}

/// Whether the card is done with a transmit descriptor.
pub fn tx_done(d: &[u8; 16]) -> bool {
    d[12] & DD != 0
}

/// The receive ring's driver side: the next descriptor to look at. The tail
/// (RDT) is the last one the card may fill; after a frame is taken, its
/// descriptor becomes the new tail.
#[derive(Debug, Default)]
pub struct RxRing {
    pub next: u16,
}

impl RxRing {
    /// The tail at start: every descriptor but one is the card's (§ 3.2.6).
    pub fn first_tail() -> u16 {
        ENTRIES - 1
    }

    /// Moves past the descriptor just taken; the tail to write.
    pub fn advance(&mut self) -> u16 {
        let tail = self.next;
        self.next = (self.next + 1) % ENTRIES;
        tail
    }
}

/// The transmit ring's driver side: where the next frame goes (TDT), and
/// the oldest descriptor the card may still hold.
#[derive(Debug, Default)]
pub struct TxRing {
    pub tail: u16,
    pub clean: u16,
}

impl TxRing {
    /// Full when one more would make the tail meet the oldest held.
    pub fn full(&self) -> bool {
        (self.tail + 1) % ENTRIES == self.clean
    }

    /// The descriptor for the next frame, and the tail to write after it.
    pub fn push(&mut self) -> Option<(u16, u16)> {
        if self.full() {
            return None;
        }
        let at = self.tail;
        self.tail = (self.tail + 1) % ENTRIES;
        Some((at, self.tail))
    }

    /// Frees descriptors from the oldest while `done` says the card has
    /// finished with them.
    pub fn reclaim(&mut self, mut done: impl FnMut(u16) -> bool) {
        while self.clean != self.tail && done(self.clean) {
            self.clean = (self.clean + 1) % ENTRIES;
        }
    }
}

#[cfg(test)]
mod tests;
