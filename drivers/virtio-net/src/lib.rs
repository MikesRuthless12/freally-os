// SPDX-License-Identifier: Apache-2.0
//! drv-virtio-net (TASK-172): a virtio network device over PCI, written from
//! the OASIS *Virtual I/O Device (VIRTIO) Version 1.2* specification. This
//! library is the driver's logic, tested on the host: the PCI capabilities
//! (§ 4.1.4), the feature bits (§ 2.2, § 5.1.3), the split virtqueue (§ 2.7)
//! and the net header (§ 5.1.6). `main.rs` puts it on a real device:
//! devmgr's resources, DMA memory, the doorbell and the interrupt.
#![cfg_attr(not(test), no_std)]

/// Red Hat's PCI vendor id, which every virtio device carries.
pub const VENDOR: u16 = 0x1AF4;

/// A virtio network device: 0x1041 is the modern id, 0x1000 the transitional
/// one (which also speaks virtio 1.x through the same capabilities).
pub fn is_net(vendor: u16, id: u16) -> bool {
    vendor == VENDOR && (id == 0x1041 || id == 0x1000)
}

/// The vendor capability's `cfg_type` (§ 4.1.4).
pub mod cap {
    pub const COMMON: u8 = 1;
    pub const NOTIFY: u8 = 2;
    pub const ISR: u8 = 3;
    pub const DEVICE: u8 = 4;
}

/// Where one configuration structure lives: a BAR, and a span inside it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Region {
    pub bar: u8,
    pub offset: u32,
    pub length: u32,
}

/// The structures a driver needs, as the function's capability list gives them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Caps {
    pub common: Option<Region>,
    pub notify: Option<Region>,
    /// The notify capability's `notify_off_multiplier`.
    pub notify_mul: u32,
    pub isr: Option<Region>,
    pub device: Option<Region>,
}

impl Caps {
    /// Whether a driver can run: the common, notify and device structures.
    pub fn usable(&self) -> bool {
        self.common.is_some() && self.notify.is_some() && self.device.is_some()
    }
}

/// Walks a function's capability list, read a byte at a time through `cfg`
/// (offsets into its configuration space), for virtio's vendor capabilities
/// (id 0x09). The first of each type is the one used (§ 4.1.4.1). The walk
/// is bounded: 48 capabilities, every pointer inside the first 256 bytes.
pub fn caps(cfg: impl Fn(u16) -> u8) -> Caps {
    let mut out = Caps::default();
    // The status register's capabilities-list bit.
    if cfg(6) & 1 << 4 == 0 {
        return out;
    }
    let le32 = |at: u16| u32::from_le_bytes([cfg(at), cfg(at + 1), cfg(at + 2), cfg(at + 3)]);
    let mut at = (cfg(0x34) & 0xFC) as u16;
    for _ in 0..48 {
        if !(0x40..=0xF0).contains(&at) {
            break;
        }
        if cfg(at) == 0x09 && cfg(at + 2) >= 16 {
            let region = Region { bar: cfg(at + 4), offset: le32(at + 8), length: le32(at + 12) };
            let slot = match cfg(at + 3) {
                cap::COMMON => Some(&mut out.common),
                cap::NOTIFY if cfg(at + 2) >= 20 => {
                    if out.notify.is_none() {
                        out.notify_mul = le32(at + 16);
                    }
                    Some(&mut out.notify)
                }
                cap::ISR => Some(&mut out.isr),
                cap::DEVICE => Some(&mut out.device),
                _ => None,
            };
            if let Some(s) = slot
                && s.is_none()
                && region.bar < 6
            {
                *s = Some(region);
            }
        }
        at = (cfg(at + 1) & 0xFC) as u16;
    }
    out
}

/// The common configuration structure's fields, as offsets (§ 4.1.4.3).
pub mod common {
    pub const DEVICE_FEATURE_SELECT: u64 = 0x00;
    pub const DEVICE_FEATURE: u64 = 0x04;
    pub const DRIVER_FEATURE_SELECT: u64 = 0x08;
    pub const DRIVER_FEATURE: u64 = 0x0C;
    pub const CONFIG_MSIX_VECTOR: u64 = 0x10;
    pub const NUM_QUEUES: u64 = 0x12;
    pub const DEVICE_STATUS: u64 = 0x14;
    pub const QUEUE_SELECT: u64 = 0x16;
    pub const QUEUE_SIZE: u64 = 0x18;
    pub const QUEUE_MSIX_VECTOR: u64 = 0x1A;
    pub const QUEUE_ENABLE: u64 = 0x1C;
    pub const QUEUE_NOTIFY_OFF: u64 = 0x1E;
    pub const QUEUE_DESC: u64 = 0x20;
    pub const QUEUE_DRIVER: u64 = 0x28;
    pub const QUEUE_DEVICE: u64 = 0x30;
    /// An MSI-X vector of none.
    pub const NO_VECTOR: u16 = 0xFFFF;
}

/// The device status bits (§ 2.1).
pub mod status {
    pub const ACKNOWLEDGE: u8 = 1;
    pub const DRIVER: u8 = 2;
    pub const DRIVER_OK: u8 = 4;
    pub const FEATURES_OK: u8 = 8;
    pub const NEEDS_RESET: u8 = 64;
    pub const FAILED: u8 = 128;
}

/// The feature bits this driver knows (§ 5.1.3, § 6).
pub mod feature {
    /// The device has a MAC address in its configuration.
    pub const NET_MAC: u64 = 1 << 5;
    /// The configuration's status field says whether the link is up.
    pub const NET_STATUS: u64 = 1 << 16;
    /// Virtio 1.x: a device that doesn't offer it is legacy, and refused.
    pub const VERSION_1: u64 = 1 << 32;
}

/// What the driver accepts of what the device `offered`: virtio 1.x is
/// required; the MAC and the link status when offered; nothing else (no
/// checksum or segmentation offloads, no merged receive buffers, so every
/// frame fits one buffer and every header is the plain 12 bytes).
pub fn negotiate(offered: u64) -> Result<u64, &'static str> {
    if offered & feature::VERSION_1 == 0 {
        return Err("the device is legacy-only (no VIRTIO_F_VERSION_1)");
    }
    Ok(offered & (feature::VERSION_1 | feature::NET_MAC | feature::NET_STATUS))
}

/// The network device's configuration (§ 5.1.4): `mac` at 0, `status` at 6.
pub mod netcfg {
    pub const MAC: u64 = 0;
    pub const STATUS: u64 = 6;
    /// `status`: the link is up.
    pub const LINK_UP: u16 = 1;
}

/// The header before every frame, with VERSION_1 and no offloads (§ 5.1.6):
/// 12 bytes, all zero on the way out (no checksum, no segmentation); on the
/// way in, `num_buffers` is 1 and the rest says nothing this driver uses.
pub const NET_HDR: usize = 12;

/// The largest Ethernet frame without its FCS, which the device strips: 1514
/// bytes (14 of header, 1500 of payload).
pub const FRAME_MAX: usize = 1514;

/// The receive queue is 0 and the transmit queue 1 (§ 5.1.2).
pub const RX: u16 = 0;
pub const TX: u16 = 1;

/// A descriptor's flags (§ 2.7.5).
const DESC_WRITE: u16 = 2;

/// Memory a queue lives in, as the device sees it. The real one is a DMA
/// page; the host tests use a byte array.
pub trait Mem {
    fn r16(&self, off: usize) -> u16;
    fn r32(&self, off: usize) -> u32;
    fn w16(&mut self, off: usize, v: u16);
    fn w32(&mut self, off: usize, v: u32);
    fn w64(&mut self, off: usize, v: u64);
    /// Orders the writes before against those after (the device reads them).
    fn fence(&self);
}

/// One split virtqueue's driver side (§ 2.7): its descriptor table, then
/// its available ring, then its used ring (4-aligned), all in one page for
/// up to 128 entries. Each descriptor is one buffer, and the caller decides
/// which descriptor carries which buffer.
#[derive(Debug)]
pub struct Queue {
    pub size: u16,
    /// The available ring's next index (free-running).
    avail: u16,
    /// The used ring's next entry to read (free-running).
    used: u16,
}

impl Queue {
    /// The most entries one page holds.
    pub const MAX: u16 = 128;

    /// A queue of `size` entries: a power of two, at most [`Queue::MAX`].
    pub fn new(size: u16) -> Option<Queue> {
        (size.is_power_of_two() && size <= Self::MAX).then_some(Queue { size, avail: 0, used: 0 })
    }

    /// The available ring's offset in the page (after 16 bytes a descriptor).
    pub fn avail_off(&self) -> usize {
        16 * self.size as usize
    }

    /// The used ring's offset: after the available ring's flags, index,
    /// entries and `used_event`, rounded up to 4.
    pub fn used_off(&self) -> usize {
        (self.avail_off() + 6 + 2 * self.size as usize).next_multiple_of(4)
    }

    /// Hands descriptor `id` to the device: `len` bytes at `addr`, which the
    /// device writes (a receive buffer) or reads (a frame to send). Seen by
    /// the device once [`Queue::publish`] runs.
    pub fn post(&mut self, m: &mut impl Mem, id: u16, addr: u64, len: u32, device_writes: bool) {
        let d = 16 * (id % self.size) as usize;
        m.w64(d, addr);
        m.w32(d + 8, len);
        m.w16(d + 12, if device_writes { DESC_WRITE } else { 0 });
        m.w16(d + 14, 0);
        let slot = self.avail_off() + 4 + 2 * (self.avail % self.size) as usize;
        m.w16(slot, id % self.size);
        self.avail = self.avail.wrapping_add(1);
    }

    /// Makes every posted descriptor visible to the device: the entries
    /// first, then the index (§ 2.7.13).
    pub fn publish(&self, m: &mut impl Mem) {
        m.fence();
        m.w16(self.avail_off() + 2, self.avail);
        m.fence();
    }

    /// The next buffer the device has finished with: its descriptor and how
    /// many bytes it wrote (a receive) or 0 (a send).
    pub fn take(&mut self, m: &impl Mem) -> Option<(u16, u32)> {
        let used_idx = m.r16(self.used_off() + 2);
        if used_idx == self.used {
            return None;
        }
        m.fence();
        let e = self.used_off() + 4 + 8 * (self.used % self.size) as usize;
        let (id, len) = (m.r32(e), m.r32(e + 4));
        self.used = self.used.wrapping_add(1);
        Some(((id % self.size as u32) as u16, len))
    }

    /// How many descriptors the device holds now.
    pub fn in_flight(&self, m: &impl Mem) -> u16 {
        self.avail.wrapping_sub(m.r16(self.used_off() + 2))
    }
}

/// A received buffer's frame: what follows the header, if the length the
/// device wrote is a whole header and a frame no longer than [`FRAME_MAX`].
pub fn frame(buffer: &[u8], written: u32) -> Option<&[u8]> {
    let n = written as usize;
    (n > NET_HDR && n <= NET_HDR + FRAME_MAX && n <= buffer.len()).then(|| &buffer[NET_HDR..n])
}

#[cfg(test)]
mod tests;
