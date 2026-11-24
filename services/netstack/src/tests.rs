// SPDX-License-Identifier: Apache-2.0
//! netstack's logic on the host: DHCP against a server played by the test,
//! the ARP cache's limits, the next hop.

use super::*;
use alloc::vec;

const ME: Mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
const S: u64 = SECOND;

/// A server's message of `kind` to `xid`: 10.0.2.15/24 from 10.0.2.2, the
/// lease `seconds` long, T1 when `t1` says.
fn server(kind: u8, xid: u32, address: Ipv4, seconds: u32, t1: Option<u32>) -> Vec<u8> {
    let mut m = vec![0u8; 240];
    m[0] = 2;
    m[4..8].copy_from_slice(&xid.to_be_bytes());
    m[16..20].copy_from_slice(&address);
    m[28..34].copy_from_slice(&ME);
    m[236..240].copy_from_slice(&[99, 130, 83, 99]);
    m.extend_from_slice(&[53, 1, kind, 54, 4, 10, 0, 2, 2, 1, 4, 255, 255, 255, 0, 3, 4, 10, 0, 2, 2, 51, 4]);
    m.extend_from_slice(&seconds.to_be_bytes());
    if let Some(t) = t1 {
        m.extend_from_slice(&[58, 4]);
        m.extend_from_slice(&t.to_be_bytes());
    }
    m.push(255);
    m
}

const GUEST: Ipv4 = [10, 0, 2, 15];

fn kind_of(m: &[u8]) -> u8 {
    m[242]
}

#[test]
fn dhcp_binds_at_the_ack_and_renews_at_t1() {
    let mut c = Client::new(9, ME, 0);
    let d = c.due(0).expect("a DISCOVER at once");
    assert_eq!(kind_of(&d), dhcp::kind::DISCOVER);
    assert_eq!(c.due(S), None, "nothing more before 2 s");
    let r = c.reply(&server(dhcp::kind::OFFER, 9, GUEST, 3600, Some(1000)), S).expect("a REQUEST for the offer");
    assert_eq!(kind_of(&r), dhcp::kind::REQUEST);
    assert_eq!(c.lease(), None, "an offer isn't an address");
    assert_eq!(c.reply(&server(dhcp::kind::ACK, 9, GUEST, 3600, Some(1000)), 2 * S), None);
    assert_eq!(c.lease().map(|l| l.address), Some(GUEST));
    assert_eq!(c.next, 2 * S + 1000 * S, "renewed at T1");
    assert_eq!(c.due(999 * S), None);
    let renew = c.due(1002 * S).expect("a REQUEST at T1");
    assert_eq!(kind_of(&renew), dhcp::kind::REQUEST);
    assert_eq!(c.lease().map(|l| l.address), Some(GUEST), "the address is kept while renewing");
    c.reply(&server(dhcp::kind::ACK, 9, GUEST, 3600, None), 1003 * S);
    assert!(matches!(c.state, Dhcp::Bound { since, .. } if since == 1003 * S));
    assert_eq!(c.next, 1003 * S + 1800 * S, "no T1 this time: half the lease");
}

#[test]
fn discover_backs_off_to_a_minute() {
    let mut c = Client::new(1, ME, 0);
    let mut sent = vec![];
    for t in 0..400 {
        if c.due(t * S).is_some() {
            sent.push(t);
        }
    }
    assert_eq!(sent, [0, 2, 6, 14, 30, 62, 126, 190, 254, 318, 382]);
}

#[test]
fn a_nak_or_a_silent_server_starts_again() {
    let mut c = Client::new(5, ME, 0);
    c.due(0);
    c.reply(&server(dhcp::kind::OFFER, 5, GUEST, 3600, None), 0);
    let again = c.reply(&server(dhcp::kind::NAK, 5, GUEST, 0, None), S).expect("a DISCOVER after the NAK");
    assert_eq!(kind_of(&again), dhcp::kind::DISCOVER);
    // Requests unanswered three times: discovering again.
    let mut c = Client::new(6, ME, 0);
    c.due(0);
    c.reply(&server(dhcp::kind::OFFER, 6, GUEST, 3600, None), 0);
    assert_eq!(c.due(2 * S).map(|m| kind_of(&m)), Some(dhcp::kind::REQUEST));
    assert_eq!(c.due(4 * S).map(|m| kind_of(&m)), Some(dhcp::kind::REQUEST));
    assert_eq!(c.due(6 * S).map(|m| kind_of(&m)), Some(dhcp::kind::DISCOVER));
}

#[test]
fn messages_out_of_turn_or_for_others_change_nothing() {
    let mut c = Client::new(7, ME, 0);
    c.due(0);
    assert_eq!(c.reply(&server(dhcp::kind::ACK, 7, GUEST, 3600, None), 0), None, "an ACK nobody asked for");
    assert_eq!(c.lease(), None);
    assert_eq!(c.reply(&server(dhcp::kind::OFFER, 8, GUEST, 3600, None), 0), None, "another transaction");
    c.reply(&server(dhcp::kind::OFFER, 7, GUEST, 3600, None), 0);
    // An ACK for an address other than the one asked for.
    c.reply(&server(dhcp::kind::ACK, 7, [10, 0, 2, 16], 3600, None), 0);
    assert_eq!(c.lease(), None);
}

#[test]
fn the_next_hop() {
    let (ours, router) = (GUEST, Some([10, 0, 2, 2]));
    assert_eq!(next_hop([10, 0, 2, 3], ours, 24, router), Some([10, 0, 2, 3]), "on the link");
    assert_eq!(next_hop([1, 1, 1, 1], ours, 24, router), router, "through the router");
    assert_eq!(next_hop([1, 1, 1, 1], ours, 24, None), None, "nowhere to send it");
    assert_eq!(next_hop([10, 0, 3, 1], ours, 16, router), Some([10, 0, 3, 1]), "a /16");
    assert_eq!(next_hop([8, 8, 8, 8], ours, 0, router), Some([8, 8, 8, 8]), "a /0 is everything");
}

#[test]
fn the_arp_cache_forgets_and_has_a_limit() {
    let mut a = ArpCache::default();
    a.learn([10, 0, 2, 2], [1; 6], 0);
    assert_eq!(a.find([10, 0, 2, 2], 299 * S), Some([1; 6]));
    assert_eq!(a.find([10, 0, 2, 2], 300 * S), None, "five minutes");
    a.learn([10, 0, 2, 2], [2; 6], 10 * S);
    assert_eq!(a.find([10, 0, 2, 2], 11 * S), Some([2; 6]), "learnt again, replaced");
    assert_eq!(a.len(), 1);
    for i in 0..100u8 {
        a.learn([10, 0, 3, i], [i; 6], 20 * S + i as u64);
    }
    assert_eq!(a.len(), ArpCache::MAX);
    assert_eq!(a.find([10, 0, 2, 2], 21 * S), None, "the oldest went first");
    assert_eq!(a.find([10, 0, 3, 99], 200 * S), Some([99; 6]));
}

mod slaac {
    use super::super::*;
    use fnet::ipv6::RouterAd;

    const IID: [u8; 8] = [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0];
    const S: u64 = SECOND;
    const FEC0: Ipv6 = [0xFE, 0xC0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    const ROUTER: Ipv6 = [0xFE, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2];

    fn global() -> Ipv6 {
        let mut a = FEC0;
        a[8..].copy_from_slice(&IID);
        a
    }

    #[test]
    fn link_local_is_checked_then_routers_asked_three_times() {
        let mut s = Slaac::new(IID, 0);
        assert_eq!(s.link_local[..2], [0xFE, 0x80]);
        assert_eq!(s.link_local[8..], IID, "the random id, never the MAC");
        assert_eq!(s.due(0), Some(Slaac6::Dad(s.link_local)));
        assert_eq!(s.due(S / 2), None, "a second's wait");
        assert!(!s.ours(&s.link_local));
        assert_eq!(s.due(S), Some(Slaac6::RouterSolicitation));
        assert!(s.ours(&s.link_local));
        assert_eq!(s.due(3 * S), None);
        assert_eq!(s.due(5 * S), Some(Slaac6::RouterSolicitation));
        assert_eq!(s.due(9 * S), Some(Slaac6::RouterSolicitation));
        assert_eq!(s.due(13 * S), None, "three, then quiet");
        assert_eq!(s.next, u64::MAX, "nothing moves while idle");
    }

    #[test]
    fn a_router_advertisement_gives_a_global_address() {
        let mut s = Slaac::new(IID, 0);
        s.due(0);
        s.due(S);
        let ad = RouterAd {
            router_lifetime_s: 1800,
            mac: Some([0x52, 0x56, 0, 0, 0, 2]),
            prefixes: alloc::vec![(FEC0, 64, 86_400)],
            dns: alloc::vec![[0xFE, 0xC0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3]],
        };
        s.router_ad(ROUTER, &ad, 2 * S);
        assert_eq!(s.router, Some((ROUTER, [0x52, 0x56, 0, 0, 0, 2])));
        assert_eq!(s.dns.len(), 1);
        assert_eq!(s.due(2 * S), Some(Slaac6::Dad(global())));
        assert_eq!(s.due(3 * S), None);
        assert!(s.ours(&global()));
        let mut gw = FEC0;
        gw[15] = 2;
        assert!(s.on_link(&gw));
        assert_eq!(s.source_for(&gw), Some(global()));
        assert_eq!(s.source_for(&ROUTER), Some(s.link_local));
        assert!(!s.on_link(&[0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]));
        // The same advertisement again adds nothing.
        s.router_ad(ROUTER, &ad, 4 * S);
        assert_eq!(s.addresses.len(), 2);
    }

    #[test]
    fn someone_holding_our_address_stops_ipv6() {
        let mut s = Slaac::new(IID, 0);
        let ll = s.link_local;
        s.due(0);
        s.advertised(&ll);
        assert_eq!(s.conflict, Some(ll));
        assert_eq!(s.due(S), None);
        assert!(!s.ours(&ll));
    }
}
