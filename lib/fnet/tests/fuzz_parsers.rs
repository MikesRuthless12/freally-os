// SPDX-License-Identifier: Apache-2.0
//! The fuzz target for every parser in fnet (TASK-185): valid frames and
//! messages (Ethernet, ARP, IPv4 with UDP, ICMP and TCP inside, DHCP, IPv6
//! with ICMPv6 and NDP, DNS), bytes changed, cut or added, then run through
//! each parser the way netstack chains them. None may panic; a panic names
//! the iteration and its bytes.
//!
//! No outside fuzzing crate (charter rule 1): a seeded xorshift drives it.
//! `cargo xtask ci -- --slow` runs it with `FREALLY_FUZZ_ITERS=2000`; the default run does 40.

use fnet::ipv6 as v6;
use fnet::tcp::{Tcp, pseudo4, pseudo6};
use fnet::{Arp, Echo, Eth, Ip, Udp, dhcp, dns};

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

const MAC: [u8; 6] = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
const A: [u8; 4] = [10, 0, 2, 15];
const B: [u8; 4] = [10, 0, 2, 2];

fn seeds() -> Vec<Vec<u8>> {
    let ll = v6::link_local([1, 2, 3, 4, 5, 6, 7, 8]);
    let router = v6::link_local([9; 8]);
    let tcp = Tcp {
        src_port: 49152,
        dst_port: 443,
        seq: 7,
        ack: 9,
        flags: fnet::tcp::flag::SYN | fnet::tcp::flag::ACK,
        window: 65535,
        mss: Some(1460),
        wscale: Some(7),
        data: b"x",
    };
    let echo = Echo { reply: false, id: 1, seq: 2, data: b"ping" }.bytes();
    let udp = fnet::udp(A, B, 68, 67, &dhcp::discover(0x1234, MAC));
    let arp = Arp { op: Arp::REQUEST, sender_mac: MAC, sender_ip: A, target_mac: [0; 6], target_ip: B }.bytes();
    let ns = v6::icmp6(&ll, &router, 135, 0, &v6::neighbor_solicitation(&router, Some(MAC)));
    let ra = {
        // A router advertisement: hop limit, flags, lifetime, two timers, then a
        // prefix information option (fec0::/64, autonomous) and a source MAC.
        let mut b = vec![64, 0, 0x07, 0x08, 0, 0, 0, 0, 0, 0, 0, 0];
        b.extend([3, 4, 64, 0xC0, 0, 0, 0x0e, 0x10, 0, 0, 0x0e, 0x10, 0, 0, 0, 0]);
        b.extend([0xfe, 0xc0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        b.extend([1, 1, 0x52, 0x54, 0, 0x12, 0x34, 0x99]);
        v6::icmp6(&router, &ll, 134, 0, &b)
    };
    let dns_reply = {
        let h = "0007818000010002000000000357575707467265616c6c79054c6f63616c0000010001c00c0005000100000e10000603776562c010c02f000100010000003c00040a000265";
        (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect::<Vec<u8>>()
    };
    vec![
        fnet::eth(MAC, MAC, fnet::ETHERTYPE_ARP, &arp),
        fnet::eth(MAC, MAC, fnet::ETHERTYPE_IPV4, &fnet::ip(A, B, fnet::PROTO_ICMP, 1, &echo)),
        fnet::eth(MAC, MAC, fnet::ETHERTYPE_IPV4, &fnet::ip(A, B, fnet::PROTO_UDP, 2, &udp)),
        fnet::eth(MAC, MAC, fnet::ETHERTYPE_IPV4, &fnet::ip(A, B, 6, 3, &tcp.bytes(pseudo4(A, B, tcp.wire_len())))),
        fnet::eth(MAC, MAC, v6::ETHERTYPE_IPV6, &v6::ip6(ll, router, v6::NEXT_ICMPV6, 255, &ns)),
        fnet::eth(MAC, MAC, v6::ETHERTYPE_IPV6, &v6::ip6(router, ll, v6::NEXT_ICMPV6, 255, &ra)),
        fnet::eth(
            MAC,
            MAC,
            v6::ETHERTYPE_IPV6,
            &v6::ip6(ll, router, 6, 64, &tcp.bytes(pseudo6(&ll, &router, tcp.wire_len()))),
        ),
        // A DNS answer over IPv6 (TASK-176: the servers RDNSS gives).
        fnet::eth(
            MAC,
            MAC,
            v6::ETHERTYPE_IPV6,
            &v6::ip6(router, ll, v6::NEXT_UDP, 64, &v6::udp6(&router, &ll, 53, 49152, &dns_reply)),
        ),
        dhcp::request(0x1234, MAC, &dhcp::Lease::default()),
        dns_reply,
    ]
}

/// Every parser over `b`, chained as netstack chains them.
fn parse_all(b: &[u8]) {
    let _ = dhcp::parse(b, 0x1234, MAC);
    if let Ok(r) = dns::parse(b) {
        let _ = dns::found(&r, "www.freally.local", dns::TYPE_A);
    }
    let Ok(e) = Eth::parse(b) else { return };
    let _ = Arp::parse(e.payload);
    if let Ok(p) = Ip::parse(e.payload) {
        let _ = Echo::parse(p.payload);
        if let Ok(u) = Udp::parse(p.src, p.dst, p.payload) {
            let _ = dhcp::parse(u.data, 0x1234, MAC);
            let _ = dns::parse(u.data);
        }
        let _ = Tcp::parse(p.payload, pseudo4(p.src, p.dst, p.payload.len()));
    }
    if let Ok(p) = v6::Ip6::parse(e.payload) {
        let _ = Tcp::parse(p.payload, pseudo6(&p.src, &p.dst, p.payload.len()));
        // DNS over IPv6 (TASK-176), as netstack chains it.
        if let Ok(u) = v6::udp6_parse(&p.src, &p.dst, p.payload) {
            let _ = dns::parse(u.data);
        }
        if let Ok((_, _, body)) = v6::icmp6_parse(&p.src, &p.dst, p.payload) {
            let _ = v6::router_ad(body);
            let _ = v6::neighbor(body);
        }
        // Past the checksum too: the ICMPv6 bodies' own readers.
        let body = p.payload.get(4..).unwrap_or(&[]);
        let _ = v6::router_ad(body);
        let _ = v6::neighbor(body);
    }
}

#[test]
fn changed_frames_never_panic() {
    let iters: u64 = std::env::var("FREALLY_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    let seeds = seeds();
    for s in &seeds {
        parse_all(s);
    }
    let mut rng = XorShift(0x5DEE_CE66_D1CE_4E5B);
    // Each iteration runs every seed once, changed.
    for i in 0..iters {
        for s in &seeds {
            let mut b = s.clone();
            match rng.next() % 4 {
                0 => (0..1 + rng.next() % 6).for_each(|_| {
                    let at = (rng.next() as usize) % b.len();
                    b[at] = rng.next() as u8;
                }),
                1 => b.truncate((rng.next() as usize) % b.len()),
                2 => {
                    let at = (rng.next() as usize) % b.len();
                    let n = 1 + rng.next() as usize % 16;
                    let extra: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
                    b.splice(at..at, extra);
                }
                _ => {
                    // A length field made huge: the first or second byte of a 16-bit field near the front.
                    let at = 14 + (rng.next() as usize) % 40;
                    if at + 1 < b.len() {
                        b[at] = 0xFF;
                        b[at + 1] = 0xFF;
                    }
                }
            }
            if std::panic::catch_unwind(|| parse_all(&b)).is_err() {
                let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
                panic!("iteration {i}: a parser panicked on {hex}");
            }
        }
    }
}

/// Changes one to six bytes, cuts, or inserts, at random.
fn mutate(rng: &mut XorShift, b: &mut Vec<u8>) {
    match rng.next() % 3 {
        0 => (0..1 + rng.next() % 6).for_each(|_| {
            let at = (rng.next() as usize) % b.len();
            b[at] = rng.next() as u8;
        }),
        1 => b.truncate((rng.next() as usize) % b.len()),
        _ => {
            let at = (rng.next() as usize) % b.len();
            let n = 1 + rng.next() as usize % 16;
            let extra: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
            b.splice(at..at, extra);
        }
    }
}

/// The same, but with each checksum made right again after the change, so
/// the changed bytes reach the code behind it: TCP's (over the pseudo-header),
/// the IPv4 header's, ICMP's; UDP's left out (0: none, which IPv4 allows);
/// and the bodies of a router advertisement and a neighbour message, which
/// have none of their own.
#[test]
fn changed_bytes_past_their_checksums_never_panic() {
    let iters: u64 = std::env::var("FREALLY_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    let mut rng = XorShift(0x2545_F491_4F6C_DD1D);
    let tcp = Tcp {
        src_port: 49152,
        dst_port: 443,
        seq: 7,
        ack: 9,
        flags: fnet::tcp::flag::ACK,
        window: 512,
        mss: Some(1460),
        wscale: Some(7),
        data: b"payload",
    }
    .bytes(pseudo4(A, B, 36));
    let echo = Echo { reply: true, id: 1, seq: 2, data: b"ping" }.bytes();
    let packet = fnet::ip(A, B, fnet::PROTO_ICMP, 5, &echo);
    let udp = fnet::udp(A, B, 53, 49999, b"a dns answer");
    let ra = [
        64u8, 0, 0x07, 0x08, 0, 0, 0, 0, 0, 0, 0, 0, 3, 4, 64, 0xC0, 0, 0, 0x0e, 0x10, 0, 0, 0x0e, 0x10, 0, 0, 0, 0,
        0xfe, 0xc0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 0x52, 0x54, 0, 0x12, 0x34, 0x99,
    ];
    let ns = v6::neighbor_solicitation(&v6::link_local([9; 8]), Some(MAC));
    let fix = |b: &mut [u8], at: usize, sum: &dyn Fn(&[u8]) -> u16| {
        if b.len() >= at + 2 {
            b[at..at + 2].fill(0);
            let s = sum(b);
            b[at..at + 2].copy_from_slice(&s.to_be_bytes());
        }
    };
    for i in 0..iters {
        let mut t = tcp.clone();
        mutate(&mut rng, &mut t);
        let len = t.len();
        fix(&mut t, 16, &|b| fnet::checksum(b, pseudo4(A, B, len)));
        let mut p = packet.clone();
        mutate(&mut rng, &mut p);
        let ihl = p.first().map_or(0, |v| (v & 0x0F) as usize * 4);
        if (20..=p.len()).contains(&ihl) {
            let (head, _) = p.split_at_mut(ihl);
            fix(head, 10, &|h| fnet::checksum(h, 0));
        }
        let mut e = echo.clone();
        mutate(&mut rng, &mut e);
        fix(&mut e, 2, &|b| fnet::checksum(b, 0));
        let mut u = udp.clone();
        mutate(&mut rng, &mut u);
        if u.len() >= 8 {
            u[6..8].fill(0);
        }
        let (mut r, mut n) = (ra.to_vec(), ns.clone());
        mutate(&mut rng, &mut r);
        mutate(&mut rng, &mut n);
        let run = || {
            let _ = Tcp::parse(&t, pseudo4(A, B, t.len()));
            if let Ok(ip) = Ip::parse(&p) {
                let _ = Echo::parse(ip.payload);
            }
            let _ = Echo::parse(&e);
            if let Ok(d) = Udp::parse(A, B, &u) {
                let _ = dns::parse(d.data);
            }
            let _ = v6::router_ad(&r);
            let _ = v6::neighbor(&n);
        };
        if std::panic::catch_unwind(run).is_err() {
            panic!("iteration {i}: a parser panicked past its checksum");
        }
    }
}
