// SPDX-License-Identifier: Apache-2.0
//! The e1000 driver's logic on the host, each value from Intel's manuals.

use super::*;

#[test]
fn the_cards() {
    for id in [0x100E, 0x100F, 0x10D3] {
        assert!(is_e1000(0x8086, id), "{id:04x}");
    }
    assert!(!is_e1000(0x8086, 0x1533), "an i210: another driver");
    assert!(!is_e1000(0x1AF4, 0x100E));
}

#[test]
fn the_register_values() {
    assert_eq!(RCTL, 0x0400_8002, "EN, BAM, SECRC; BSIZE 00 is 2048");
    assert_eq!(TCTL, 0x0004_00FA, "EN, PSP, CT 0x0F, COLD 0x40");
    assert_eq!(TIPG, 0x0060_200A);
    assert_eq!(TX_CMD, 0x0B, "EOP, IFCS, RS");
}

#[test]
fn the_mac_from_ral_and_rah() {
    // QEMU's 52:54:00:12:34:56, as the card holds it.
    assert_eq!(mac_of(0x1200_5452, 0x8000_5634), Some([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]));
    assert_eq!(mac_of(0x1200_5452, 0x0000_5634), None, "not valid");
    assert_eq!(mac_of(0, 0x8000_0000), None, "all zero");
    assert_eq!(mac_of(0x0000_0001, 0x8000_0000), None, "a multicast address");
}

#[test]
fn descriptors() {
    let t = tx_desc(0x1234_5678_9ABC, 60);
    assert_eq!(&t[..8], &0x1234_5678_9ABCu64.to_le_bytes());
    assert_eq!((t[8], t[9], t[11], t[12]), (60, 0, 0x0B, 0));
    assert!(!tx_done(&t));
    let mut done = t;
    done[12] = DD;
    assert!(tx_done(&done));
    let r = rx_desc(0x8000);
    assert_eq!(rx_done(&r), None, "the card hasn't written it");
    let mut w = r;
    w[8..10].copy_from_slice(&60u16.to_le_bytes());
    w[12] = DD | EOP;
    assert_eq!(rx_done(&w), Some((60, true)));
    w[13] = 0x80;
    assert_eq!(rx_done(&w), Some((60, false)), "an error: not taken");
    w[13] = 0;
    w[12] = DD;
    assert_eq!(rx_done(&w), Some((60, false)), "not the end of a packet");
}

#[test]
fn the_receive_ring_walks_and_wraps() {
    assert_eq!(RxRing::first_tail(), ENTRIES - 1);
    let mut r = RxRing::default();
    for i in 0..(3 * ENTRIES as u32) {
        assert_eq!(r.advance() as u32, i % ENTRIES as u32);
    }
    assert_eq!(r.next, 0);
}

#[test]
fn the_transmit_ring_fills_and_drains() {
    let mut t = TxRing::default();
    for i in 0..ENTRIES - 1 {
        assert_eq!(t.push(), Some((i, i + 1)));
    }
    assert!(t.full(), "one short of the whole ring");
    assert_eq!(t.push(), None);
    // The card finishes the first ten.
    t.reclaim(|d| d < 10);
    assert_eq!(t.clean, 10);
    for _ in 0..10 {
        assert!(t.push().is_some());
    }
    assert_eq!(t.push(), None);
    assert_eq!(t.tail, 9, "wrapped");
    // Everything finished: empty again, never past the tail.
    t.reclaim(|_| true);
    assert_eq!(t.clean, t.tail);
}
