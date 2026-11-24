// SPDX-License-Identifier: Apache-2.0
//! netstack (TASK-174, TASK-175, FR-060): IPv4 and IPv6 over the network
//! device, as one service. It takes the device's frames (netdev), gets an
//! IPv4 address by DHCP and IPv6 ones by SLAAC, answers ARP and NDP for its
//! addresses and echo requests to them, and serves the net protocol (PRD
//! § 4) to the clients the Network page lists. With no network device it
//! says so and ends.
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::{FErr, signal};
use fnet::ipv6::{self as v6, Ip6, Ipv6, kind};
use fnet::{Arp, Echo, Eth, Ip, Ipv4, Mac, Udp};
use fstd::ipc::{Channel, Client};
use fstd::time::{Duration, Instant};
use fwire::proto::conductor as c;
use fwire::proto::net as nt;
use fwire::proto::netdev as nd;
use svc_netstack::{ArpCache, Client as Dhcp, Neighbors, Slaac, Slaac6, next_hop};

fstd::program!(run);

/// The echo id netstack's pings carry.
const PING_ID: u16 = 0x4652;
/// An ARP (or NDP) request's retry.
const ARP_RETRY: u64 = 500_000_000;

/// Who may use the network today (TASK-183 makes this the Network page's list).
fn allowed(client: &str) -> bool {
    matches!(client, "terminal" | "settings")
        || (cfg!(feature = "flavour-e2e") && matches!(client, "conductor" | "cdpd"))
}

/// Where a ping goes: the address, and the neighbour it goes through.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dest {
    V4 { to: Ipv4, hop: Ipv4 },
    V6 { to: Ipv6, hop: Ipv6 },
}

/// A ping waiting for its neighbour's MAC, then for its echo.
struct Pending {
    client: usize,
    txid: u32,
    ordinal: u32,
    dest: Dest,
    seq: u16,
    sent: Option<u64>,
    arp_at: u64,
    deadline: u64,
}

struct Stack {
    dev: Client,
    mac: Mac,
    driver: String,
    link: bool,
    dhcp: Dhcp,
    arp: ArpCache,
    slaac: Slaac,
    nd6: Neighbors<Ipv6>,
    /// IPv6 addresses already said in the log.
    v6_said: usize,
    ip_id: u16,
    pending: Vec<Pending>,
}

fn now() -> u64 {
    Instant::now().as_ns()
}

fn dotted(a: Ipv4) -> String {
    alloc::format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3])
}

impl Stack {
    fn send(&mut self, frames: Vec<Vec<u8>>) {
        let _ = self.dev.call::<_, nd::Sent>(
            nd::SEND,
            &nd::Frames { frames },
            Vec::new(),
            Instant::after(Duration::from_secs(1)),
        );
    }

    fn ours(&self) -> Option<(Ipv4, u8, Option<Ipv4>)> {
        self.dhcp.lease().map(|l| (l.address, l.prefix, l.router))
    }

    /// An IPv4 packet to `dst` at `mac`.
    fn send_ip(&mut self, mac: Mac, src: Ipv4, dst: Ipv4, proto: u8, payload: &[u8]) {
        self.ip_id = self.ip_id.wrapping_add(1);
        let packet = fnet::ip(src, dst, proto, self.ip_id, payload);
        let frame = fnet::eth(mac, self.mac, fnet::ETHERTYPE_IPV4, &packet);
        self.send(alloc::vec![frame]);
    }

    /// An ICMPv6 message to `dst` at `mac` (NDP's go out with 255 hops left).
    fn send_icmp6(&mut self, mac: Mac, src: Ipv6, dst: Ipv6, kind: u8, body: &[u8]) {
        let m = v6::icmp6(&src, &dst, kind, 0, body);
        let packet = v6::ip6(src, dst, v6::NEXT_ICMPV6, 255, &m);
        let frame = fnet::eth(mac, self.mac, v6::ETHERTYPE_IPV6, &packet);
        self.send(alloc::vec![frame]);
    }

    fn arp_request(&mut self, from: Ipv4, target: Ipv4) {
        let a = Arp { op: Arp::REQUEST, sender_mac: self.mac, sender_ip: from, target_mac: [0; 6], target_ip: target };
        let frame = fnet::eth(fnet::BROADCAST, self.mac, fnet::ETHERTYPE_ARP, &a.bytes());
        self.send(alloc::vec![frame]);
    }

    /// A neighbour solicitation for `target`, to its solicited-node group.
    fn ndp_request(&mut self, from: Ipv6, target: Ipv6) {
        let group = v6::solicited_node(&target);
        let body = v6::neighbor_solicitation(&target, Some(self.mac));
        self.send_icmp6(v6::multicast_mac(&group), from, group, kind::NEIGHBOR_SOLICITATION, &body);
    }

    /// DHCP's next message, when due: broadcast from 0.0.0.0 (or our own
    /// address while renewing).
    fn dhcp_due(&mut self) {
        let t = now();
        let had = self.ours().map(|o| o.0);
        if let Some(m) = self.dhcp.due(t) {
            self.dhcp_send(&m, had);
        }
    }

    fn dhcp_send(&mut self, m: &[u8], from: Option<Ipv4>) {
        let src = from.unwrap_or([0; 4]);
        let datagram = fnet::udp(src, [255; 4], fnet::dhcp::CLIENT_PORT, fnet::dhcp::SERVER_PORT, m);
        self.send_ip(fnet::BROADCAST, src, [255; 4], fnet::PROTO_UDP, &datagram);
    }

    /// SLAAC's next step, when due; each new address said once.
    fn slaac_due(&mut self) {
        match self.slaac.due(now()) {
            Some(Slaac6::Dad(a)) => {
                let group = v6::solicited_node(&a);
                let body = v6::neighbor_solicitation(&a, None);
                self.send_icmp6(v6::multicast_mac(&group), v6::UNSPECIFIED, group, kind::NEIGHBOR_SOLICITATION, &body);
            }
            Some(Slaac6::RouterSolicitation) => {
                let (ll, body) = (self.slaac.link_local, v6::router_solicitation(Some(self.mac)));
                self.send_icmp6(
                    v6::multicast_mac(&v6::ALL_ROUTERS),
                    ll,
                    v6::ALL_ROUTERS,
                    kind::ROUTER_SOLICITATION,
                    &body,
                );
            }
            None => {}
        }
        if let Some(a) = self.slaac.conflict.take() {
            fstd::warn!("netstack: {} is someone else's on this link: IPv6 is off", v6::text(&a));
        }
        while self.v6_said < self.slaac.addresses.len() {
            let (a, p) = self.slaac.addresses[self.v6_said];
            self.v6_said += 1;
            let router = self.slaac.router.map_or(String::from("none"), |r| v6::text(&r.0));
            fstd::info!("netstack: {}/{} by SLAAC, router {}", v6::text(&a), p, router);
        }
    }

    /// One frame the device received.
    fn frame(&mut self, f: &[u8], clients: &[Channel]) {
        let Ok(e) = Eth::parse(f) else { return };
        // Ours, a broadcast, or an IPv6 multicast (33:33:…).
        if e.dst != self.mac && e.dst != fnet::BROADCAST && e.dst[..2] != [0x33, 0x33] {
            return;
        }
        let ours = self.ours().map(|o| o.0);
        match e.ethertype {
            fnet::ETHERTYPE_ARP => {
                let Ok(a) = Arp::parse(e.payload) else { return };
                if Some(a.target_ip) == ours || self.arp.find(a.sender_ip, now()).is_some() {
                    self.arp.learn(a.sender_ip, a.sender_mac, now());
                }
                if a.op == Arp::REPLY && a.target_mac == self.mac {
                    self.arp.learn(a.sender_ip, a.sender_mac, now());
                }
                if a.op == Arp::REQUEST && Some(a.target_ip) == ours {
                    let r = Arp {
                        op: Arp::REPLY,
                        sender_mac: self.mac,
                        sender_ip: a.target_ip,
                        target_mac: a.sender_mac,
                        target_ip: a.sender_ip,
                    };
                    let frame = fnet::eth(a.sender_mac, self.mac, fnet::ETHERTYPE_ARP, &r.bytes());
                    self.send(alloc::vec![frame]);
                }
                self.pings(clients);
            }
            fnet::ETHERTYPE_IPV4 => {
                let Ok(p) = Ip::parse(e.payload) else { return };
                let to_us = Some(p.dst) == ours || p.dst == [255; 4];
                match p.proto {
                    fnet::PROTO_ICMP if Some(p.dst) == ours => self.icmp(&e, &p, clients),
                    fnet::PROTO_UDP if to_us || ours.is_none() => {
                        let Ok(u) = Udp::parse(p.src, p.dst, p.payload) else { return };
                        if u.dst_port == fnet::dhcp::CLIENT_PORT && u.src_port == fnet::dhcp::SERVER_PORT {
                            let had = ours.is_some();
                            if let Some(m) = self.dhcp.reply(u.data, now()) {
                                let from = self.ours().map(|o| o.0);
                                self.dhcp_send(&m, from);
                            }
                            if !had && let Some(l) = self.dhcp.lease() {
                                fstd::info!(
                                    "netstack: {}/{} from {} by DHCP, router {}, DNS {}, lease {} s",
                                    dotted(l.address),
                                    l.prefix,
                                    dotted(l.server),
                                    l.router.map_or(String::from("none"), dotted),
                                    l.dns.first().copied().map_or(String::from("none"), dotted),
                                    l.seconds
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
            v6::ETHERTYPE_IPV6 => self.ipv6(&e, clients),
            _ => {}
        }
    }

    fn icmp(&mut self, e: &Eth, p: &Ip, clients: &[Channel]) {
        let Ok(echo) = Echo::parse(p.payload) else { return };
        if !echo.reply {
            let r = Echo { reply: true, id: echo.id, seq: echo.seq, data: echo.data }.bytes();
            self.send_ip(e.src, p.dst, p.src, fnet::PROTO_ICMP, &r);
            return;
        }
        if echo.id == PING_ID {
            self.answered(|d| matches!(d, Dest::V4 { to, .. } if *to == p.src), echo.seq, clients);
        }
    }

    /// An IPv6 packet: ICMPv6 alone (echo, and NDP's four messages).
    fn ipv6(&mut self, e: &Eth, clients: &[Channel]) {
        let Ok(p) = Ip6::parse(e.payload) else { return };
        if p.next != v6::NEXT_ICMPV6 {
            return;
        }
        let Ok((k, _, body)) = v6::icmp6_parse(&p.src, &p.dst, p.payload) else { return };
        let t = now();
        match k {
            kind::ECHO_REQUEST if self.slaac.ours(&p.dst) => {
                self.send_icmp6(e.src, p.dst, p.src, kind::ECHO_REPLY, body);
            }
            kind::ECHO_REPLY if body.len() >= 4 && u16::from_be_bytes([body[0], body[1]]) == PING_ID => {
                let seq = u16::from_be_bytes([body[2], body[3]]);
                self.answered(|d| matches!(d, Dest::V6 { to, .. } if *to == p.src), seq, clients);
            }
            // Only a router on this link (its link-local address, 255 hops) is listened to.
            kind::ROUTER_ADVERTISEMENT if p.hops == 255 && p.src[..2] == [0xFE, 0x80] => {
                if let Ok(ad) = v6::router_ad(body) {
                    if let Some(m) = ad.mac {
                        self.nd6.learn(p.src, m, t);
                    }
                    self.slaac.router_ad(p.src, &ad, t);
                }
            }
            kind::NEIGHBOR_SOLICITATION if p.hops == 255 => {
                let Ok((target, mac)) = v6::neighbor(body) else { return };
                if let Some(m) = mac
                    && p.src != v6::UNSPECIFIED
                {
                    self.nd6.learn(p.src, m, t);
                }
                if self.slaac.ours(&target) {
                    // From :: (another's DAD): to every node; else to the one who asked.
                    let (dst, dmac) = if p.src == v6::UNSPECIFIED {
                        let all = [0xFF, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
                        (all, v6::multicast_mac(&all))
                    } else {
                        (p.src, mac.unwrap_or(e.src))
                    };
                    let na = v6::neighbor_advertisement(&target, self.mac, p.src != v6::UNSPECIFIED);
                    self.send_icmp6(dmac, target, dst, kind::NEIGHBOR_ADVERTISEMENT, &na);
                }
            }
            kind::NEIGHBOR_ADVERTISEMENT if p.hops == 255 => {
                let Ok((target, mac)) = v6::neighbor(body) else { return };
                self.slaac.advertised(&target);
                if let Some(m) = mac {
                    self.nd6.learn(target, m, t);
                }
                self.pings(clients);
            }
            _ => {}
        }
    }

    /// The ping `matches` a reply from, with this `seq`: its round trip to its client.
    fn answered(&mut self, matches: impl Fn(&Dest) -> bool, seq: u16, clients: &[Channel]) {
        let t = now();
        if let Some(i) = self.pending.iter().position(|q| q.sent.is_some() && matches(&q.dest) && q.seq == seq) {
            let q = self.pending.remove(i);
            let rtt = (t - q.sent.unwrap_or(t)) / 1000;
            if let Some(ch) = clients.get(q.client)
                && let Ok(m) = fwire::reply(q.txid, q.ordinal, &nt::Pong { rtt_us: rtt.min(u32::MAX as u64) as u32 })
            {
                let _ = ch.send(&m, Vec::new());
            }
        }
    }

    /// Each waiting ping: sent once its neighbour is known, its ARP (or
    /// NDP) asked again, or given up at its deadline.
    fn pings(&mut self, clients: &[Channel]) {
        let t = now();
        let mut i = 0;
        while i < self.pending.len() {
            let q = &self.pending[i];
            if t >= q.deadline {
                let q = self.pending.remove(i);
                if let Some(ch) = clients.get(q.client)
                    && let Ok(m) = fwire::error_reply(q.txid, q.ordinal, FErr::TimedOut as i32, "no answer")
                {
                    let _ = ch.send(&m, Vec::new());
                }
                continue;
            }
            if q.sent.is_none() {
                let (dest, seq, ask) = (q.dest, q.seq, t >= q.arp_at);
                let data = [0x46u8; 32];
                match dest {
                    Dest::V4 { to, hop } => {
                        let Some((src, _, _)) = self.ours() else {
                            i += 1;
                            continue;
                        };
                        match self.arp.find(hop, t) {
                            Some(mac) => {
                                let echo = Echo { reply: false, id: PING_ID, seq, data: &data }.bytes();
                                self.send_ip(mac, src, to, fnet::PROTO_ICMP, &echo);
                                self.pending[i].sent = Some(t);
                            }
                            None if ask => {
                                self.pending[i].arp_at = t + ARP_RETRY;
                                self.arp_request(src, hop);
                            }
                            None => {}
                        }
                    }
                    Dest::V6 { to, hop } => {
                        let Some(src) = self.slaac.source_for(&to) else {
                            i += 1;
                            continue;
                        };
                        match self.nd6.find(hop, t) {
                            Some(mac) => {
                                let mut body = Vec::with_capacity(36);
                                body.extend_from_slice(&PING_ID.to_be_bytes());
                                body.extend_from_slice(&seq.to_be_bytes());
                                body.extend_from_slice(&data);
                                self.send_icmp6(mac, src, to, kind::ECHO_REQUEST, &body);
                                self.pending[i].sent = Some(t);
                            }
                            None if ask => {
                                self.pending[i].arp_at = t + ARP_RETRY;
                                let from = self.slaac.source_for(&hop).unwrap_or(src);
                                self.ndp_request(from, hop);
                            }
                            None => {}
                        }
                    }
                }
            }
            i += 1;
        }
    }

    fn next_wake(&self) -> u64 {
        let mut t = self.dhcp.next.min(self.slaac.next);
        for q in &self.pending {
            t = t.min(q.deadline);
            if q.sent.is_none() {
                t = t.min(q.arp_at);
            }
        }
        t
    }

    fn interfaces(&self) -> nt::Interfaces {
        let l = self.dhcp.lease();
        nt::Interfaces {
            list: alloc::vec![nt::Interface {
                name: String::from("eth0"),
                driver: self.driver.clone(),
                mac: self.mac,
                link: self.link,
                ipv4: l.map(|l| (l.address, l.prefix)),
                router: l.and_then(|l| l.router),
                dns: l.map(|l| l.dns.clone()).unwrap_or_default(),
                lease_s: l.map_or(0, |l| l.seconds),
                ipv6: self.slaac.addresses.clone(),
                router6: self.slaac.router.map(|r| r.0),
                dns6: self.slaac.dns.clone(),
            }],
        }
    }

    /// A ping asked for: queued for its neighbour, or refused with why.
    fn ping(
        &mut self,
        client: usize,
        txid: u32,
        ordinal: u32,
        dest: Option<Dest>,
        seq: u16,
        timeout_ms: u32,
    ) -> Result<(), (FErr, &'static str)> {
        let dest = dest.ok_or((FErr::NotFound, "no route"))?;
        let t = now();
        let wait = timeout_ms.clamp(1, 10_000) as u64 * 1_000_000;
        self.pending.push(Pending { client, txid, ordinal, dest, seq, sent: None, arp_at: t, deadline: t + wait });
        Ok(())
    }
}

fn open() -> Result<(Stack, Channel), String> {
    // Whichever driver found its card (one without a card ends at once).
    let ch = ["drv-virtio-net", "drv-e1000"]
        .iter()
        .find_map(|d| fstd::service::connect_within(d, Duration::from_secs(5)).ok())
        .ok_or_else(|| String::from("no network device"))?;
    let mut dev = Client::new(ch);
    let deadline = || Instant::after(Duration::from_secs(2));
    let (info, _) =
        dev.call::<_, nd::Info>(nd::INFO, &(), Vec::new(), deadline()).map_err(|e| alloc::format!("{e:?}"))?;
    let (_, hs) = dev
        .call::<_, nd::Subscribed>(nd::SUBSCRIBE, &(), Vec::new(), deadline())
        .map_err(|e| alloc::format!("{e:?}"))?;
    let events = Channel(hs.into_iter().next().ok_or("no events channel")?);
    // DHCP's transaction id, and IPv6's interface id: random, never the MAC.
    let mut r = [0u8; 12];
    frandom::fill(&mut r).map_err(|_| String::from("no randomness (RDRAND/RDSEED refused)"))?;
    let xid = u32::from_le_bytes([r[0], r[1], r[2], r[3]]);
    let iid: [u8; 8] = r[4..].try_into().map_err(|_| String::from("no randomness"))?;
    let stack = Stack {
        dev,
        mac: info.mac,
        driver: info.driver,
        link: info.link,
        dhcp: Dhcp::new(xid, info.mac, now()),
        arp: ArpCache::default(),
        slaac: Slaac::new(iid, now()),
        nd6: Neighbors::default(),
        v6_said: 0,
        ip_id: 0,
        pending: Vec::new(),
    };
    Ok((stack, events))
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    let (mut s, events) = match open() {
        Ok(x) => x,
        Err(e) if e == "no network device" => {
            fstd::info!("netstack: no network device: nothing to do");
            return 0;
        }
        Err(e) => {
            fstd::error!("netstack: {e}");
            return 1;
        }
    };
    let Ok((listen, give)) = Channel::pair() else { return 1 };
    if fstd::service::register(give).is_err() {
        return 1;
    }
    fstd::info!("netstack: up on {} (eth0), asking DHCP and SLAAC", s.driver);
    let mut clients: Vec<Channel> = Vec::new();
    loop {
        s.dhcp_due();
        s.slaac_due();
        s.pings(&clients);
        let mut items = alloc::vec![[listen.0.raw(), signal::READABLE], [events.0.raw(), signal::READABLE]];
        items.extend(clients.iter().map(|ch| [ch.0.raw(), signal::READABLE | signal::PEER_CLOSED]));
        match fstd::sys::wait_many(&items, s.next_wake()) {
            Ok(0) => {
                while let Ok(m) = listen.try_recv() {
                    if let Ok((h, p)) = fwire::parse(&m.bytes)
                        && h.ordinal == c::INCOMING
                        && let Ok(inc) = fwire::from_payload::<c::Incoming>(p)
                        && let Some(ch) = m.handles.into_iter().next()
                    {
                        if allowed(&inc.client) {
                            clients.push(Channel(ch));
                        } else {
                            fstd::warn!("netstack: {} may not use the network (not on the Network page)", inc.client);
                        }
                    }
                }
            }
            Ok(1) => {
                while let Ok(m) = events.try_recv() {
                    let Ok((h, p)) = fwire::parse(&m.bytes) else { continue };
                    match h.ordinal {
                        nd::EV_FRAMES => {
                            if let Ok(f) = fwire::from_payload::<nd::Frames>(p) {
                                for frame in &f.frames {
                                    s.frame(frame, &clients);
                                }
                            }
                        }
                        nd::EV_LINK => {
                            if let Ok(l) = fwire::from_payload::<nd::Link>(p) {
                                s.link = l.up;
                            }
                        }
                        _ => {}
                    }
                }
            }
            Ok(k) => {
                if !serve(&mut s, &clients[k - 2], k - 2) {
                    clients.remove(k - 2);
                    // Its pings go with it; the later clients' indices move down.
                    s.pending.retain(|q| q.client != k - 2);
                    for q in &mut s.pending {
                        if q.client > k - 2 {
                            q.client -= 1;
                        }
                    }
                }
            }
            Err(FErr::TimedOut) => {}
            Err(_) => fstd::time::sleep(Duration::from_millis(10)),
        }
    }
}

/// Answers every request waiting from client `i`; false once it's gone.
fn serve(s: &mut Stack, ch: &Channel, i: usize) -> bool {
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
        let asked = match h.ordinal {
            nt::INTERFACES => Ok(Some(fwire::reply(h.txid, h.ordinal, &s.interfaces()))),
            nt::PING => match (fwire::from_payload::<nt::Ping>(p), s.ours()) {
                (Err(_), _) => Err((FErr::InvalidArgs, "unreadable")),
                (Ok(_), None) => Err((FErr::BadState, "no address yet")),
                (Ok(ping), Some((ours, prefix, router))) => {
                    let dest = next_hop(ping.to, ours, prefix, router).map(|hop| Dest::V4 { to: ping.to, hop });
                    s.ping(i, h.txid, h.ordinal, dest, ping.seq, ping.timeout_ms).map(|_| None)
                }
            },
            nt::PING6 => match fwire::from_payload::<nt::Ping6>(p) {
                Err(_) => Err((FErr::InvalidArgs, "unreadable")),
                Ok(ping) if s.slaac.source_for(&ping.to).is_none() => Err((FErr::BadState, "no IPv6 address yet")),
                Ok(ping) => {
                    let hop = if s.slaac.on_link(&ping.to) { Some(ping.to) } else { s.slaac.router.map(|r| r.0) };
                    let dest = hop.map(|hop| Dest::V6 { to: ping.to, hop });
                    s.ping(i, h.txid, h.ordinal, dest, ping.seq, ping.timeout_ms).map(|_| None)
                }
            },
            _ => Err((FErr::NotSupported, "not served")),
        };
        let reply = match asked {
            Ok(Some(r)) => r,
            // A ping: answered when its echo comes back, or at its deadline.
            Ok(None) => continue,
            Err((e, why)) => fwire::error_reply(h.txid, h.ordinal, e as i32, why),
        };
        if let Ok(m) = reply {
            let _ = ch.send(&m, Vec::new());
        }
    }
}
