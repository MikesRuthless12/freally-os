// SPDX-License-Identifier: Apache-2.0
//! The wire formats on the host: each built and read back, each worked
//! against bytes written out by hand from the RFCs, each malformed case
//! refused with its reason.

use super::*;
use alloc::vec;

const ME: Mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
const GW: Mac = [0x52, 0x55, 10, 0, 2, 2];
const GUEST: Ipv4 = [10, 0, 2, 15];
const GATEWAY: Ipv4 = [10, 0, 2, 2];

#[test]
fn the_internet_checksum() {
    // RFC 1071 § 3's example: 00 01 f2 03 f4 f5 f6 f7 sums to ddf2.
    assert_eq!(checksum(&[0x00, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7], 0), !0xddf2);
    // An odd length pads with a zero byte.
    assert_eq!(checksum(&[0xAB], 0), !0xAB00);
    // A header with its checksum in place sums to 0.
    let p = ip(GUEST, GATEWAY, PROTO_UDP, 7, &[]);
    assert_eq!(checksum(&p[..20], 0), 0);
}

#[test]
fn a_known_ipv4_header() {
    // A header written out by hand: 45 00 0073 0000 4000 4011 [b861] c0a8 0001 c0a8 00c7.
    let mut h = vec![
        0x45, 0x00, 0x00, 0x73, 0x00, 0x00, 0x40, 0x00, 0x40, 0x11, 0xb8, 0x61, 0xc0, 0xa8, 0x00, 0x01, 0xc0, 0xa8,
        0x00, 0xc7,
    ];
    h.resize(0x73, 0);
    let p = Ip::parse(&h).unwrap();
    assert_eq!((p.src, p.dst, p.proto, p.ttl, p.payload.len()), ([192, 168, 0, 1], [192, 168, 0, 199], 17, 64, 95));
    assert_eq!(ip([192, 168, 0, 1], [192, 168, 0, 199], 17, 0, &h[20..])[..20], h[..20]);
}

#[test]
fn bad_ipv4_is_refused_with_its_reason() {
    let good = ip(GUEST, GATEWAY, PROTO_ICMP, 1, &[1, 2, 3, 4]);
    assert!(Ip::parse(&good).is_ok());
    assert_eq!(Ip::parse(&good[..19]), Err(Bad::Short));
    let mut v6 = good.clone();
    v6[0] = 0x65;
    assert_eq!(Ip::parse(&v6), Err(Bad::Version));
    let mut ihl = good.clone();
    ihl[0] = 0x44;
    assert_eq!(Ip::parse(&ihl), Err(Bad::HeaderLength));
    let mut total = good.clone();
    total[3] = 200;
    assert_eq!(Ip::parse(&total), Err(Bad::TotalLength));
    let mut sum = good.clone();
    sum[8] = 1;
    assert_eq!(Ip::parse(&sum), Err(Bad::Checksum));
    // A fragment (more fragments set), its checksum made right.
    let mut frag = good.clone();
    frag[6] = 0x20;
    frag[10..12].copy_from_slice(&[0, 0]);
    let s = checksum(&frag[..20], 0);
    frag[10..12].copy_from_slice(&s.to_be_bytes());
    assert_eq!(Ip::parse(&frag), Err(Bad::Fragment));
    // Padding past the total length isn't payload.
    let mut padded = good.clone();
    padded.extend_from_slice(&[0; 10]);
    assert_eq!(Ip::parse(&padded).unwrap().payload, &[1, 2, 3, 4]);
}

#[test]
fn an_echo_and_its_reply() {
    let req = Echo { reply: false, id: 0x1234, seq: 7, data: b"freally" }.bytes();
    assert_eq!(&req[..2], &[8, 0]);
    let e = Echo::parse(&req).unwrap();
    assert_eq!((e.reply, e.id, e.seq, e.data), (false, 0x1234, 7, &b"freally"[..]));
    let rep = Echo { reply: true, id: e.id, seq: e.seq, data: e.data }.bytes();
    assert!(Echo::parse(&rep).unwrap().reply);
    let mut bad = rep.clone();
    bad[9] ^= 1;
    assert_eq!(Echo::parse(&bad), Err(Bad::Checksum));
    // Destination unreachable is ICMP, but not an echo.
    let mut unreach = vec![3, 1, 0, 0, 0, 0, 0, 0];
    let s = checksum(&unreach, 0);
    unreach[2..4].copy_from_slice(&s.to_be_bytes());
    assert_eq!(Echo::parse(&unreach), Err(Bad::Unsupported));
}

#[test]
fn udp_with_its_pseudo_header() {
    let d = udp(GUEST, GATEWAY, 5000, 53, b"query");
    let u = Udp::parse(GUEST, GATEWAY, &d).unwrap();
    assert_eq!((u.src_port, u.dst_port, u.data), (5000, 53, &b"query"[..]));
    // The checksum covers the addresses: the same bytes from elsewhere fail.
    assert_eq!(Udp::parse([10, 0, 2, 16], GATEWAY, &d), Err(Bad::Checksum));
    // A checksum of 0 means none was made.
    let mut none = d.clone();
    none[6..8].copy_from_slice(&[0, 0]);
    assert!(Udp::parse([1, 2, 3, 4], GATEWAY, &none).is_ok());
    let mut long = d.clone();
    long[5] = 200;
    assert_eq!(Udp::parse(GUEST, GATEWAY, &long), Err(Bad::TotalLength));
    assert_eq!(Udp::parse(GUEST, GATEWAY, &d[..7]), Err(Bad::Short));
}

#[test]
fn arp_both_ways() {
    let a = Arp { op: Arp::REQUEST, sender_mac: ME, sender_ip: GUEST, target_mac: [0; 6], target_ip: GATEWAY };
    let f = eth(BROADCAST, ME, ETHERTYPE_ARP, &a.bytes());
    assert_eq!(f.len(), 60, "padded to Ethernet's least");
    let e = Eth::parse(&f).unwrap();
    assert_eq!((e.dst, e.src, e.ethertype), (BROADCAST, ME, ETHERTYPE_ARP));
    assert_eq!(Arp::parse(e.payload), Ok(a));
    let mut ipv6 = a.bytes();
    ipv6[2..4].copy_from_slice(&[0x86, 0xDD]);
    assert_eq!(Arp::parse(&ipv6), Err(Bad::Unsupported));
    assert_eq!(Arp::parse(&a.bytes()[..27]), Err(Bad::Short));
    assert_eq!(Eth::parse(&f[..13]), Err(Bad::Short));
}

/// A server's message as QEMU's user network sends it: 10.0.2.15 offered,
/// mask 255.255.255.0, router 10.0.2.2, DNS 10.0.2.3, a day's lease.
fn qemu_reply(kind: u8, xid: u32) -> Vec<u8> {
    let mut m = vec![0u8; 240];
    m[0] = 2;
    m[1] = 1;
    m[2] = 6;
    m[4..8].copy_from_slice(&xid.to_be_bytes());
    m[16..20].copy_from_slice(&GUEST);
    m[20..24].copy_from_slice(&GATEWAY);
    m[28..34].copy_from_slice(&ME);
    m[236..240].copy_from_slice(&[99, 130, 83, 99]);
    m.extend_from_slice(&[53, 1, kind, 54, 4, 10, 0, 2, 2, 1, 4, 255, 255, 255, 0, 3, 4, 10, 0, 2, 2]);
    m.extend_from_slice(&[6, 4, 10, 0, 2, 3, 51, 4, 0, 1, 0x51, 0x80, 0, 0, 255]);
    m
}

#[test]
fn dhcp_discover_offer_request_ack() {
    let d = dhcp::discover(0xC0FFEE, ME);
    assert_eq!((d[0], d[10], &d[28..34], &d[236..240]), (1, 0x80, &ME[..], &[99, 130, 83, 99][..]));
    assert_eq!(&d[240..243], &[53, 1, dhcp::kind::DISCOVER]);
    let lease = match dhcp::parse(&qemu_reply(dhcp::kind::OFFER, 0xC0FFEE), 0xC0FFEE, ME) {
        Ok(dhcp::Reply::Offer(l)) => l,
        other => panic!("{other:?}"),
    };
    assert_eq!(lease.address, GUEST);
    assert_eq!(lease.server, GATEWAY);
    assert_eq!(lease.prefix, 24);
    assert_eq!(lease.router, Some(GATEWAY));
    assert_eq!(lease.dns, [[10, 0, 2, 3]]);
    assert_eq!((lease.seconds, lease.renew), (86_400, 43_200), "no T1: half the lease");
    let r = dhcp::request(0xC0FFEE, ME, &lease);
    assert_eq!(&r[240..252], &[53, 1, dhcp::kind::REQUEST, 50, 4, 10, 0, 2, 15, 54, 4, 10]);
    assert!(matches!(dhcp::parse(&qemu_reply(dhcp::kind::ACK, 0xC0FFEE), 0xC0FFEE, ME), Ok(dhcp::Reply::Ack(_))));
    assert_eq!(dhcp::parse(&qemu_reply(dhcp::kind::NAK, 0xC0FFEE), 0xC0FFEE, ME), Ok(dhcp::Reply::Nak));
}

#[test]
fn a_dhcp_message_not_for_this_client_or_malformed_is_refused() {
    let m = qemu_reply(dhcp::kind::OFFER, 7);
    assert_eq!(dhcp::parse(&m, 8, ME), Err(Bad::Unsupported), "another transaction");
    assert_eq!(dhcp::parse(&m, 7, GW), Err(Bad::Unsupported), "another client");
    assert_eq!(dhcp::parse(&m[..239], 7, ME), Err(Bad::Short));
    // An option whose length runs past the message.
    let mut cut = m.clone();
    cut.truncate(cut.len() - 1);
    cut.extend_from_slice(&[6, 200, 1, 2]);
    assert_eq!(dhcp::parse(&cut, 7, ME), Err(Bad::Short));
    // No message type.
    let mut untyped = m.clone();
    untyped[240..243].copy_from_slice(&[0, 0, 0]);
    assert_eq!(dhcp::parse(&untyped, 7, ME), Err(Bad::Unsupported));
}
