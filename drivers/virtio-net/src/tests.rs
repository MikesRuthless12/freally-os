// SPDX-License-Identifier: Apache-2.0
//! The driver's logic on the host, each case worked out from the virtio 1.2
//! specification and from what QEMU 10's virtio-net-pci shows a guest.

use super::*;

/// A page of DMA memory, little-endian, as the device sees it.
struct Page(std::vec::Vec<u8>);

impl Page {
    fn new() -> Page {
        Page(vec![0; 4096])
    }
}

impl Mem for Page {
    fn r16(&self, o: usize) -> u16 {
        u16::from_le_bytes(self.0[o..o + 2].try_into().unwrap())
    }
    fn r32(&self, o: usize) -> u32 {
        u32::from_le_bytes(self.0[o..o + 4].try_into().unwrap())
    }
    fn w16(&mut self, o: usize, v: u16) {
        self.0[o..o + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn w32(&mut self, o: usize, v: u32) {
        self.0[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn w64(&mut self, o: usize, v: u64) {
        self.0[o..o + 8].copy_from_slice(&v.to_le_bytes());
    }
    fn fence(&self) {}
}

/// The device's side of a queue: it reads the available ring and answers
/// in the used ring, as QEMU does.
struct Device {
    seen: u16,
    used: u16,
}

impl Device {
    /// Takes every newly available descriptor; answers each with `len`.
    /// Returns the descriptors, with their address, length and flags.
    fn serve(&mut self, q: &Queue, m: &mut Page, len: u32) -> std::vec::Vec<(u16, u64, u32, u16)> {
        let mut out = std::vec::Vec::new();
        let avail = m.r16(q.avail_off() + 2);
        while self.seen != avail {
            let id = m.r16(q.avail_off() + 4 + 2 * (self.seen % q.size) as usize);
            let d = 16 * id as usize;
            let addr = u64::from_le_bytes(m.0[d..d + 8].try_into().unwrap());
            out.push((id, addr, m.r32(d + 8), m.r16(d + 12)));
            let e = q.used_off() + 4 + 8 * (self.used % q.size) as usize;
            m.w32(e, id as u32);
            m.w32(e + 4, len);
            self.used = self.used.wrapping_add(1);
            self.seen = self.seen.wrapping_add(1);
        }
        m.w16(q.used_off() + 2, self.used);
        out
    }
}

/// A configuration space laid out as QEMU 10's virtio-net-pci lays out its
/// own: MSI-X first (0x98), then the vendor capabilities, common (0x88),
/// ISR (0x78), device (0x68), notify (0x54, 20 bytes, multiplier 4) and PCI
/// configuration access (0x40, 20 bytes), all in BAR 4.
fn qemu_config() -> std::vec::Vec<u8> {
    let mut c = vec![0u8; 256];
    c[6] = 1 << 4;
    c[0x34] = 0x98;
    c[0x98] = 0x11;
    c[0x99] = 0x88;
    let mut vendor = |at: usize, next: u8, len: u8, kind: u8, offset: u32, length: u32| {
        c[at] = 0x09;
        c[at + 1] = next;
        c[at + 2] = len;
        c[at + 3] = kind;
        c[at + 4] = 4;
        c[at + 8..at + 12].copy_from_slice(&offset.to_le_bytes());
        c[at + 12..at + 16].copy_from_slice(&length.to_le_bytes());
    };
    vendor(0x88, 0x78, 16, cap::COMMON, 0x0000, 0x1000);
    vendor(0x78, 0x68, 16, cap::ISR, 0x1000, 0x1000);
    vendor(0x68, 0x54, 16, cap::DEVICE, 0x2000, 0x1000);
    vendor(0x54, 0x40, 20, cap::NOTIFY, 0x3000, 0x1000);
    vendor(0x40, 0x00, 20, 5, 0, 0);
    c[0x54 + 16..0x54 + 20].copy_from_slice(&4u32.to_le_bytes());
    c
}

#[test]
fn qemus_capabilities_are_found() {
    let c = qemu_config();
    let found = caps(|o| c[o as usize]);
    assert_eq!(found.common, Some(Region { bar: 4, offset: 0, length: 0x1000 }));
    assert_eq!(found.isr, Some(Region { bar: 4, offset: 0x1000, length: 0x1000 }));
    assert_eq!(found.device, Some(Region { bar: 4, offset: 0x2000, length: 0x1000 }));
    assert_eq!(found.notify, Some(Region { bar: 4, offset: 0x3000, length: 0x1000 }));
    assert_eq!(found.notify_mul, 4);
    assert!(found.usable());
}

#[test]
fn a_broken_capability_list_ends_the_walk() {
    // No list at all.
    let mut c = qemu_config();
    c[6] = 0;
    assert_eq!(caps(|o| c[o as usize]), Caps::default());
    // A list that points back at itself is walked a bounded number of times.
    let mut c = qemu_config();
    c[0x89] = 0x88;
    let found = caps(|o| c[o as usize]);
    assert!(found.common.is_some() && found.device.is_none());
    // A pointer into the header, or a BAR that doesn't exist, is ignored.
    let mut c = qemu_config();
    c[0x34] = 0x10;
    assert_eq!(caps(|o| c[o as usize]), Caps::default());
    let mut c = qemu_config();
    c[0x88 + 4] = 7;
    assert!(caps(|o| c[o as usize]).common.is_none());
    // A notify capability too short to hold its multiplier isn't taken.
    let mut c = qemu_config();
    c[0x56] = 16;
    assert!(caps(|o| c[o as usize]).notify.is_none());
}

#[test]
fn the_first_capability_of_a_type_wins() {
    let mut c = qemu_config();
    // A second common structure after the first, in BAR 2.
    c[0x41] = 0xC0;
    c[0xC0] = 0x09;
    c[0xC2] = 16;
    c[0xC3] = cap::COMMON;
    c[0xC4] = 2;
    assert_eq!(caps(|o| c[o as usize]).common.map(|r| r.bar), Some(4));
}

#[test]
fn only_virtio_1_is_accepted_and_only_what_the_driver_knows() {
    assert!(negotiate(feature::NET_MAC | feature::NET_STATUS).is_err(), "legacy-only");
    let all = u64::MAX;
    assert_eq!(negotiate(all), Ok(feature::VERSION_1 | feature::NET_MAC | feature::NET_STATUS));
    assert_eq!(negotiate(feature::VERSION_1), Ok(feature::VERSION_1));
}

#[test]
fn the_network_device_ids() {
    assert!(is_net(0x1AF4, 0x1041) && is_net(0x1AF4, 0x1000));
    assert!(!is_net(0x1AF4, 0x1042), "a block device");
    assert!(!is_net(0x8086, 0x1041));
}

#[test]
fn a_queue_fits_one_page() {
    let q = Queue::new(128).unwrap();
    assert_eq!(q.avail_off(), 2048);
    assert_eq!(q.used_off(), 2312, "2048 + 6 + 256 = 2310, rounded up to 4");
    assert!(q.used_off() + 6 + 8 * 128 <= 4096);
    assert!(Queue::new(256).is_none(), "more than a page");
    assert!(Queue::new(48).is_none(), "not a power of two");
}

#[test]
fn posted_buffers_come_back_in_the_used_ring() {
    let mut m = Page::new();
    let mut q = Queue::new(8).unwrap();
    let mut dev = Device { seen: 0, used: 0 };
    for id in 0..8u16 {
        q.post(&mut m, id, 0x10_0000 + id as u64 * 2048, 2048, true);
    }
    assert!(dev.serve(&q, &mut m, 60).is_empty(), "nothing is seen before publish");
    q.publish(&mut m);
    let seen = dev.serve(&q, &mut m, 60);
    assert_eq!(seen.len(), 8);
    assert_eq!(seen[3], (3, 0x10_0000 + 3 * 2048, 2048, 2), "a receive buffer: the device writes it");
    for id in 0..8 {
        assert_eq!(q.take(&m), Some((id, 60)));
    }
    assert_eq!(q.take(&m), None);
    // A frame to send: the device only reads it.
    q.post(&mut m, 5, 0x20_0000, 74, false);
    q.publish(&mut m);
    assert_eq!(dev.serve(&q, &mut m, 0), [(5, 0x20_0000, 74, 0)]);
}

#[test]
fn the_rings_indices_wrap_past_65535() {
    let mut m = Page::new();
    let mut q = Queue::new(4).unwrap();
    let mut dev = Device { seen: 0, used: 0 };
    for n in 0..70_000u32 {
        let id = (n % 4) as u16;
        q.post(&mut m, id, n as u64, 1, true);
        q.publish(&mut m);
        assert_eq!(q.in_flight(&m), 1);
        dev.serve(&q, &mut m, n);
        assert_eq!(q.take(&m), Some((id, n)), "round {n}");
        assert_eq!(q.in_flight(&m), 0);
    }
}

#[test]
fn a_frame_is_the_bytes_after_the_header() {
    let mut buf = vec![0u8; 2048];
    buf[NET_HDR..NET_HDR + 6].copy_from_slice(&[0xFF; 6]);
    assert_eq!(frame(&buf, (NET_HDR + 60) as u32).map(<[u8]>::len), Some(60));
    assert_eq!(frame(&buf, (NET_HDR + 60) as u32).unwrap()[..6], [0xFF; 6]);
    assert_eq!(frame(&buf, NET_HDR as u32), None, "a header and nothing else");
    assert_eq!(frame(&buf, (NET_HDR + FRAME_MAX + 1) as u32), None, "longer than any frame");
    assert_eq!(frame(&buf[..100], 200), None, "longer than the buffer");
}
