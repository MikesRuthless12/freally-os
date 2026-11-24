// SPDX-License-Identifier: Apache-2.0
//! The resolver on the host: its queries, its retries and servers, TC to
//! TCP, CNAME chains, the cache, and replies that aren't its own ignored.

use alloc::vec::Vec;

use fnet::dns::{TYPE_A, TYPE_AAAA, TYPE_CNAME};

use crate::resolver::{Answer, Resolver, Send};

const S: u64 = 1_000_000_000;
const ONE: [u8; 4] = [10, 0, 3, 1];
const TWO: [u8; 4] = [10, 0, 3, 2];

fn name(n: &str) -> Vec<u8> {
    let mut v = Vec::new();
    for l in n.split('.') {
        v.push(l.len() as u8);
        v.extend_from_slice(l.as_bytes());
    }
    v.push(0);
    v
}

/// A response (no compression): `rcode`, TC, and answers (name, type, TTL, data).
fn reply(id: u16, q: &str, qtype: u16, rcode: u8, tc: bool, answers: &[(&str, u16, u32, Vec<u8>)]) -> Vec<u8> {
    let flags: u16 = 0x8180 | rcode as u16 | if tc { 0x0200 } else { 0 };
    let mut m = Vec::new();
    for v in [id, flags, 1, answers.len() as u16, 0, 0] {
        m.extend_from_slice(&v.to_be_bytes());
    }
    m.extend(name(q));
    m.extend_from_slice(&qtype.to_be_bytes());
    m.extend_from_slice(&1u16.to_be_bytes());
    for (n, t, ttl, data) in answers {
        m.extend(name(n));
        m.extend_from_slice(&t.to_be_bytes());
        m.extend_from_slice(&1u16.to_be_bytes());
        m.extend_from_slice(&ttl.to_be_bytes());
        m.extend_from_slice(&(data.len() as u16).to_be_bytes());
        m.extend_from_slice(data);
    }
    m
}

/// The one UDP query in `out`: (server, port, its id).
fn udp(out: &[Send]) -> (fnet::Ipv4, u16, u16) {
    match out {
        [Send::Udp { server, port, bytes }] => (*server, *port, u16::from_be_bytes([bytes[0], bytes[1]])),
        other => panic!("not one UDP query: {other:?}"),
    }
}

fn resolver(servers: &[fnet::Ipv4]) -> Resolver {
    let mut r = Resolver::new();
    r.servers = servers.to_vec();
    r
}

#[test]
fn an_answer_then_the_cache() {
    let mut r = resolver(&[ONE]);
    let mut out = Vec::new();
    let id = r.lookup("Test.Freally.Local", TYPE_A, 0, [0xAB, 0xCD, 0x12, 0x34], &mut out).unwrap_err();
    let (server, port, txid) = udp(&out);
    assert_eq!((server, txid), (ONE, 0xABCD));
    assert!((49152..=65535).contains(&port));
    assert_eq!(r.ports().collect::<Vec<_>>(), [port]);
    // Not ours: another id, another question, another server, another port.
    let good =
        reply(txid, "test.freally.local", TYPE_A, 0, false, &[("test.freally.local", TYPE_A, 300, vec![10, 0, 3, 1])]);
    let mut wrong = good.clone();
    wrong[1] ^= 1;
    assert!(r.udp(ONE, port, &wrong, S / 10, [0; 4], &mut out).is_empty());
    let other_q = reply(txid, "other.freally.local", TYPE_A, 0, false, &[]);
    assert!(r.udp(ONE, port, &other_q, S / 10, [0; 4], &mut out).is_empty());
    assert!(r.udp(TWO, port, &good, S / 10, [0; 4], &mut out).is_empty());
    assert!(r.udp(ONE, port ^ 1, &good, S / 10, [0; 4], &mut out).is_empty());
    let done = r.udp(ONE, port, &good, S / 10, [0; 4], &mut out);
    assert_eq!(done, [(id, Answer::Found(vec![vec![10, 0, 3, 1]]))]);
    // Cached for its TTL, then asked again.
    out.clear();
    assert_eq!(
        r.lookup("test.freally.local", TYPE_A, 299 * S, [0; 4], &mut out),
        Ok(Answer::Found(vec![vec![10, 0, 3, 1]]))
    );
    assert!(out.is_empty());
    assert!(r.lookup("test.freally.local", TYPE_A, 301 * S, [0; 4], &mut out).is_err());
}

#[test]
fn retries_then_the_next_server_then_given_up() {
    let mut r = resolver(&[ONE, TWO]);
    let mut out = Vec::new();
    let id = r.lookup("slow.freally.local", TYPE_A, 0, [1, 1, 1, 1], &mut out).unwrap_err();
    let mut sent = Vec::new();
    let mut now = 0;
    let mut done = Vec::new();
    while done.is_empty() {
        for s in out.drain(..) {
            if let Send::Udp { server, .. } = s {
                sent.push((now / S, server));
            }
        }
        now = r.next_wake().unwrap();
        done = r.due(now, || [2, 2, 2, 2], &mut out);
    }
    // 0 s and 1 s to the first; 3 s and 4 s to the second; 6 s and 7 s to the first; given up at 10 s.
    assert_eq!(sent, [(0, ONE), (1, ONE), (3, TWO), (4, TWO), (6, ONE), (7, ONE), (9, TWO)]);
    assert_eq!((done, now / S), (vec![(id, Answer::NoAnswer)], 10));
    let mut r = resolver(&[]);
    assert_eq!(r.lookup("any.freally.local", TYPE_A, 0, [0; 4], &mut out), Ok(Answer::NoServer), "no server");
    assert_eq!(r.lookup("a..b", TYPE_A, 0, [0; 4], &mut out), Ok(Answer::Failed), "not a name");
}

#[test]
fn truncated_then_tcp() {
    let mut r = resolver(&[ONE]);
    let mut out = Vec::new();
    let id = r.lookup("big.freally.local", TYPE_A, 0, [7, 7, 7, 7], &mut out).unwrap_err();
    let (_, port, txid) = udp(&out);
    out.clear();
    assert!(
        r.udp(ONE, port, &reply(txid, "big.freally.local", TYPE_A, 0, true, &[]), S / 10, [0; 4], &mut out).is_empty()
    );
    let Send::Tcp { id: tid, server, bytes } = &out[0] else { panic!("{out:?}") };
    assert_eq!((*tid, *server, u16::from_be_bytes([bytes[0], bytes[1]])), (id, ONE, txid));
    let many: Vec<(&str, u16, u32, Vec<u8>)> =
        (1..=40).map(|i| ("big.freally.local", TYPE_A, 60, vec![10, 0, 4, i])).collect();
    let done = r.tcp(id, Some(&reply(txid, "big.freally.local", TYPE_A, 0, false, &many)), S / 5, [0; 4], &mut out);
    let Answer::Found(a) = &done[0].1 else { panic!("{done:?}") };
    assert_eq!(a.len(), 40);
    // A TCP connection that fails: the lookup fails.
    let id2 = r.lookup("big2.freally.local", TYPE_A, 0, [8, 8, 8, 8], &mut out).unwrap_err();
    let (_, port, txid) = udp(&out[1..]);
    out.clear();
    r.udp(ONE, port, &reply(txid, "big2.freally.local", TYPE_A, 0, true, &[]), S / 10, [0; 4], &mut out);
    assert_eq!(r.tcp(id2, None, S, [0; 4], &mut out), [(id2, Answer::Failed)]);
}

#[test]
fn chains_nxdomain_and_nodata() {
    let mut r = resolver(&[ONE]);
    let mut out = Vec::new();
    // www → cdn.example.net, whose A comes in a second query with a new id and port.
    let id = r.lookup("www.freally.local", TYPE_AAAA, 0, [1, 2, 3, 4], &mut out).unwrap_err();
    let (_, port, txid) = udp(&out);
    out.clear();
    let first = reply(
        txid,
        "www.freally.local",
        TYPE_AAAA,
        0,
        false,
        &[("www.freally.local", TYPE_CNAME, 30, name("cdn.example.net"))],
    );
    assert!(r.udp(ONE, port, &first, S / 10, [5, 6, 7, 8], &mut out).is_empty());
    let (_, port2, txid2) = udp(&out);
    assert_ne!((port2, txid2), (port, txid), "a fresh id and port for the alias");
    let six = vec![0x20, 1, 0xd, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
    let second =
        reply(txid2, "cdn.example.net", TYPE_AAAA, 0, false, &[("cdn.example.net", TYPE_AAAA, 300, six.clone())]);
    assert_eq!(r.udp(ONE, port2, &second, S / 5, [0; 4], &mut out), [(id, Answer::Found(vec![six]))]);
    // Cached for the chain's smallest TTL (30 s).
    assert!(r.lookup("www.freally.local", TYPE_AAAA, 29 * S, [0; 4], &mut out).is_ok());
    assert!(r.lookup("www.freally.local", TYPE_AAAA, 31 * S, [9, 9, 9, 9], &mut out).is_err());
    // NXDOMAIN, kept a minute; NODATA, the same.
    out.clear();
    let id = r.lookup("none.freally.local", TYPE_A, 0, [3, 3, 3, 3], &mut out).unwrap_err();
    let (_, port, txid) = udp(&out);
    assert_eq!(
        r.udp(ONE, port, &reply(txid, "none.freally.local", TYPE_A, 3, false, &[]), S, [0; 4], &mut out),
        [(id, Answer::NoSuchName)]
    );
    assert_eq!(r.lookup("none.freally.local", TYPE_A, 59 * S, [0; 4], &mut out), Ok(Answer::NoSuchName));
    out.clear();
    let id = r.lookup("v4only.freally.local", TYPE_AAAA, 0, [4, 4, 4, 4], &mut out).unwrap_err();
    let (_, port, txid) = udp(&out);
    let nodata = reply(
        txid,
        "v4only.freally.local",
        TYPE_AAAA,
        0,
        false,
        &[("v4only.freally.local", TYPE_A, 60, vec![1, 2, 3, 4])],
    );
    assert_eq!(r.udp(ONE, port, &nodata, S, [0; 4], &mut out), [(id, Answer::NoData)]);
}

#[test]
fn a_chain_past_eight_aliases_fails() {
    let mut r = resolver(&[ONE]);
    let mut out = Vec::new();
    let id = r.lookup("a0.x", TYPE_A, 0, [0, 1, 0, 1], &mut out).unwrap_err();
    for k in 0..=8u8 {
        let (_, port, txid) = udp(&out);
        out.clear();
        let (from, to) = (alloc::format!("a{k}.x"), alloc::format!("a{}.x", k + 1));
        let done = r.udp(
            ONE,
            port,
            &reply(txid, &from, TYPE_A, 0, false, &[(&from, TYPE_CNAME, 60, name(&to))]),
            S,
            [k, 2, k, 2],
            &mut out,
        );
        if k == 8 {
            assert_eq!(done, [(id, Answer::Failed)]);
        } else {
            assert!(done.is_empty(), "alias {k}");
        }
    }
}
