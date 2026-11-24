// SPDX-License-Identifier: Apache-2.0
//! netstack (TASK-174, FR-060): IPv4 over the network device, as one
//! service. It takes the device's frames (netdev), gets an address by DHCP,
//! answers ARP for that address and echo requests to it, and serves the net
//! protocol (PRD § 4) to the clients the Network page lists. With no network
//! device it says so and ends.
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::{FErr, signal};
use fnet::{Arp, Echo, Eth, Ip, Ipv4, Mac, Udp};
use fstd::ipc::{Channel, Client};
use fstd::time::{Duration, Instant};
use fwire::proto::conductor as c;
use fwire::proto::net as nt;
use fwire::proto::netdev as nd;
use svc_netstack::{ArpCache, Client as Dhcp, next_hop};

fstd::program!(run);

/// The echo id netstack's pings carry.
const PING_ID: u16 = 0x4652;
/// An ARP request's retry, and how long a ping waits for its neighbour.
const ARP_RETRY: u64 = 500_000_000;

/// Who may use the network today (TASK-183 makes this the Network page's list).
fn allowed(client: &str) -> bool {
    matches!(client, "terminal" | "settings")
        || (cfg!(feature = "flavour-e2e") && matches!(client, "conductor" | "cdpd"))
}

/// A ping waiting for its neighbour's MAC, then for its echo.
struct Pending {
    client: usize,
    txid: u32,
    to: Ipv4,
    hop: Ipv4,
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

    fn arp_request(&mut self, from: Ipv4, target: Ipv4) {
        let a = Arp { op: Arp::REQUEST, sender_mac: self.mac, sender_ip: from, target_mac: [0; 6], target_ip: target };
        let frame = fnet::eth(fnet::BROADCAST, self.mac, fnet::ETHERTYPE_ARP, &a.bytes());
        self.send(alloc::vec![frame]);
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

    /// One frame the device received.
    fn frame(&mut self, f: &[u8], clients: &[Channel]) {
        let Ok(e) = Eth::parse(f) else { return };
        if e.dst != self.mac && e.dst != fnet::BROADCAST {
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
        if echo.id != PING_ID {
            return;
        }
        let t = now();
        if let Some(i) = self.pending.iter().position(|q| q.sent.is_some() && q.to == p.src && q.seq == echo.seq) {
            let q = self.pending.remove(i);
            let rtt = (t - q.sent.unwrap_or(t)) / 1000;
            if let Some(ch) = clients.get(q.client)
                && let Ok(m) = fwire::reply(q.txid, nt::PING, &nt::Pong { rtt_us: rtt.min(u32::MAX as u64) as u32 })
            {
                let _ = ch.send(&m, Vec::new());
            }
        }
    }

    /// Each waiting ping: sent once its neighbour is known, its ARP asked
    /// again, or given up at its deadline.
    fn pings(&mut self, clients: &[Channel]) {
        let t = now();
        let Some((src, _, _)) = self.ours() else { return };
        let mut i = 0;
        while i < self.pending.len() {
            let q = &self.pending[i];
            if t >= q.deadline {
                let q = self.pending.remove(i);
                if let Some(ch) = clients.get(q.client)
                    && let Ok(m) = fwire::error_reply(q.txid, nt::PING, FErr::TimedOut as i32, "no answer")
                {
                    let _ = ch.send(&m, Vec::new());
                }
                continue;
            }
            if q.sent.is_none() {
                let (hop, to, seq) = (q.hop, q.to, q.seq);
                match self.arp.find(hop, t) {
                    Some(mac) => {
                        let data = [0x46u8; 32];
                        let echo = Echo { reply: false, id: PING_ID, seq, data: &data }.bytes();
                        self.send_ip(mac, src, to, fnet::PROTO_ICMP, &echo);
                        self.pending[i].sent = Some(t);
                    }
                    None if t >= self.pending[i].arp_at => {
                        self.pending[i].arp_at = t + ARP_RETRY;
                        self.arp_request(src, hop);
                    }
                    None => {}
                }
            }
            i += 1;
        }
    }

    fn next_wake(&self) -> u64 {
        let mut t = self.dhcp.next;
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
            }],
        }
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
    let mut r = [0u8; 4];
    frandom::fill(&mut r).map_err(|_| String::from("no randomness (RDRAND/RDSEED refused)"))?;
    let dhcp = Dhcp::new(u32::from_le_bytes(r), info.mac, now());
    let stack = Stack {
        dev,
        mac: info.mac,
        driver: info.driver,
        link: info.link,
        dhcp,
        arp: ArpCache::default(),
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
    fstd::info!("netstack: up on {} (eth0), asking DHCP", s.driver);
    let mut clients: Vec<Channel> = Vec::new();
    loop {
        s.dhcp_due();
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
        let reply = match h.ordinal {
            nt::INTERFACES => fwire::reply(h.txid, h.ordinal, &s.interfaces()),
            nt::PING => match (fwire::from_payload::<nt::Ping>(p), s.ours()) {
                (Err(_), _) => fwire::error_reply(h.txid, h.ordinal, FErr::InvalidArgs as i32, "unreadable"),
                (Ok(_), None) => fwire::error_reply(h.txid, h.ordinal, FErr::BadState as i32, "no address yet"),
                (Ok(ping), Some((ours, prefix, router))) => match next_hop(ping.to, ours, prefix, router) {
                    None => fwire::error_reply(h.txid, h.ordinal, FErr::NotFound as i32, "no route"),
                    Some(hop) => {
                        let t = now();
                        let wait = ping.timeout_ms.clamp(1, 10_000) as u64 * 1_000_000;
                        s.pending.push(Pending {
                            client: i,
                            txid: h.txid,
                            to: ping.to,
                            hop,
                            seq: ping.seq,
                            sent: None,
                            arp_at: t,
                            deadline: t + wait,
                        });
                        continue;
                    }
                },
            },
            _ => fwire::error_reply(h.txid, h.ordinal, FErr::NotSupported as i32, "not served"),
        };
        if let Ok(m) = reply {
            let _ = ch.send(&m, Vec::new());
        }
    }
}
