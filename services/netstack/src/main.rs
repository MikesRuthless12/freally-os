// SPDX-License-Identifier: Apache-2.0
//! netstack (TASK-174, TASK-175, TASK-177, FR-060): IPv4 and IPv6 over the
//! network device, as one service. It takes the device's frames (netdev),
//! gets an IPv4 address by DHCP and IPv6 ones by SLAAC, answers ARP and NDP
//! for its addresses and echo requests to them, makes outgoing TCP
//! connections over IPv4 (a RST for any segment of none), resolves names by
//! DNS (UDP, TCP when truncated: `resolver.rs`), and serves the net
//! protocol (PRD § 4) to the clients the Network page lists. With no network
//! device it says so and ends.
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::{FErr, signal};
use fnet::ipv6::{self as v6, Ip6, Ipv6, kind};
use fnet::tcp::{PROTO_TCP, Tcp, flag, pseudo4};
use fnet::{Arp, Echo, Eth, Ip, Ipv4, Mac, Udp};
use fstd::ipc::{Channel, Client};
use fstd::time::{Duration, Instant};
use fwire::proto::conductor as c;
use fwire::proto::net as nt;
use fwire::proto::netdev as nd;
use svc_netstack::resolver::{self, Answer, Ids, Resolver, Server};
use svc_netstack::tcp::{self as tcp, Ended, Out, State as TcpState, Tcb};
use svc_netstack::{ArpCache, Client as Dhcp, Neighbors, Slaac, Slaac6, next_hop};

fstd::program!(run);

/// The echo id netstack's pings carry.
const PING_ID: u16 = 0x4652;
/// An ARP (or NDP) request's retry.
const ARP_RETRY: u64 = 500_000_000;
/// The MSS offered over IPv4 on Ethernet: 1,500 − 20 − 20.
const MSS4: u16 = 1460;
/// Connections a client may hold at once.
const SOCKETS_PER_CLIENT: usize = svc_netstack::limits::SOCKETS_PER_CLIENT;
/// Connections a listener holds not yet accepted, at most.
#[cfg(feature = "flavour-e2e")]
const BACKLOG: usize = svc_netstack::limits::BACKLOG;
/// Pings and DNS lookups a client may have waiting at once.
const PINGS_PER_CLIENT: usize = svc_netstack::limits::PINGS_PER_CLIENT;
const LOOKUPS_PER_CLIENT: usize = svc_netstack::limits::LOOKUPS_PER_CLIENT;
/// The ephemeral ports (RFC 6335 § 6).
const EPHEMERAL: core::ops::RangeInclusive<u16> = 49152..=65535;

/// Who may use the network: the programs the Network page lists (TASK-183).
fn allowed(client: &str) -> bool {
    svc_netstack::grants::allowed(client, cfg!(feature = "flavour-e2e"))
}

/// Where a ping goes: the address, and the neighbour it goes through.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dest {
    V4 { to: Ipv4, hop: Ipv4 },
    V6 { to: Ipv6, hop: Ipv6 },
}

/// A reply owed to a client: its request's txid and ordinal.
#[derive(Clone, Copy)]
struct Owed {
    txid: u32,
    ordinal: u32,
}

/// A TCP connection, and what its client is waiting on.
struct Socket {
    id: u32,
    client: usize,
    local_port: u16,
    to: Ipv4,
    port: u16,
    hop: Ipv4,
    /// The next hop's MAC: until it's known the connection doesn't start.
    mac: Option<Mac>,
    arp_at: u64,
    tcb: Tcb,
    /// CONNECT's reply, owed until the connection opens, is refused, or passes this deadline.
    connecting: Option<(Owed, u64)>,
    /// A RECV waiting for data: the most to give, and until when.
    reading: Option<(Owed, usize, u64)>,
    /// A SEND waiting for room: its bytes, and how many are taken.
    writing: Option<(Owed, Vec<u8>, usize)>,
    /// Its client closed it: it goes once the connection has (TIME-WAIT included).
    closed: bool,
    /// The resolver's, not a client's: its lookup, the framed query (sent
    /// once it opens), and the answer as it arrives.
    dns: Option<(u32, Vec<u8>, Vec<u8>)>,
    /// A listener took it and its client hasn't accepted it yet (TASK-184).
    waiting: bool,
}

/// A port a test client listens on (TASK-184, e2e images only).
#[cfg(feature = "flavour-e2e")]
struct Listener {
    client: usize,
    port: u16,
    backlog: usize,
    /// ACCEPT's reply, owed until a connection is open or this deadline.
    accepting: Option<(Owed, u64)>,
}

fn answer<T: fwire::Wire>(ch: Option<&Channel>, o: Owed, r: &T) {
    if let Some(ch) = ch
        && let Ok(m) = fwire::reply(o.txid, o.ordinal, r)
    {
        let _ = ch.send(&m, Vec::new());
    }
}

fn refuse(ch: Option<&Channel>, o: Owed, e: FErr, why: &str) {
    if let Some(ch) = ch
        && let Ok(m) = fwire::error_reply(o.txid, o.ordinal, e as i32, why)
    {
        let _ = ch.send(&m, Vec::new());
    }
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
    sockets: Vec<Socket>,
    next_socket: u32,
    next_port: u16,
    /// RFC 6528's secret, for initial sequence numbers.
    isn_key: [u8; 32],
    /// Each lookup's id and port.
    ids: Ids,
    resolver: Resolver,
    /// Lookups clients are waiting on: (the resolver's id, the client, its reply).
    resolving: Vec<(u32, usize, Owed)>,
    /// Each client's name, as `clients` holds them (whose a connection is).
    names: Vec<String>,
    #[cfg(feature = "flavour-e2e")]
    listeners: Vec<Listener>,
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

    /// Frames in as few messages as the netdev protocol allows (32 frames, 60 KiB).
    fn send_frames(&mut self, frames: Vec<Vec<u8>>) {
        let (mut batch, mut bytes) = (Vec::new(), 0);
        for f in frames {
            if batch.len() == 32 || bytes + f.len() > 60 * 1024 {
                self.send(core::mem::take(&mut batch));
                bytes = 0;
            }
            bytes += f.len();
            batch.push(f);
        }
        if !batch.is_empty() {
            self.send(batch);
        }
    }

    /// A TCP segment's frame, from `src`:`local_port` to `to`:`port` through `mac`.
    fn tcp_frame(&mut self, mac: Mac, src: Ipv4, local_port: u16, to: Ipv4, port: u16, o: &Out) -> Vec<u8> {
        let seg = Tcp {
            src_port: local_port,
            dst_port: port,
            seq: o.seq,
            ack: o.ack,
            flags: o.flags,
            window: o.window,
            mss: o.mss,
            wscale: o.wscale,
            data: &o.data,
        };
        self.ip_id = self.ip_id.wrapping_add(1);
        let packet = fnet::ip(src, to, PROTO_TCP, self.ip_id, &seg.bytes(pseudo4(src, to, seg.wire_len())));
        fnet::eth(mac, self.mac, fnet::ETHERTYPE_IPV4, &packet)
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
                    PROTO_TCP if Some(p.dst) == ours => self.tcp_in(&e, &p),
                    fnet::PROTO_UDP if to_us || ours.is_none() => {
                        let Ok(u) = Udp::parse(p.src, p.dst, p.payload) else { return };
                        if u.src_port == fnet::dns::PORT
                            && Some(p.dst) == ours
                            && self.resolver.ports().any(|q| q == u.dst_port)
                        {
                            let mut sends = Vec::new();
                            let done = self.resolver.udp(
                                Server::V4(p.src),
                                u.dst_port,
                                u.data,
                                now(),
                                self.ids.draw(),
                                &mut sends,
                            );
                            self.dns_send(sends);
                            self.resolved(done, clients);
                            return;
                        }
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

    /// A TCP segment to us: its connection's, else answered with a RST (RFC 9293 § 3.10.7.1).
    fn tcp_in(&mut self, e: &Eth, p: &Ip) {
        let Ok(seg) = Tcp::parse(p.payload, pseudo4(p.src, p.dst, p.payload.len())) else { return };
        let t = now();
        if let Some(s) =
            self.sockets.iter_mut().find(|s| s.to == p.src && s.port == seg.src_port && s.local_port == seg.dst_port)
        {
            s.tcb.segment(&seg, t);
            return;
        }
        #[cfg(feature = "flavour-e2e")]
        if self.syn_to_listener(e, p, &seg, t) {
            return;
        }
        if seg.flags & flag::RST != 0 {
            return; // never a RST for a RST
        }
        let o = if seg.flags & flag::ACK != 0 {
            Out { seq: seg.ack, ack: 0, flags: flag::RST, window: 0, mss: None, wscale: None, data: Vec::new() }
        } else {
            let ack = seg.seq.wrapping_add(seg.seq_len());
            Out { seq: 0, ack, flags: flag::RST | flag::ACK, window: 0, mss: None, wscale: None, data: Vec::new() }
        };
        let f = self.tcp_frame(e.src, p.dst, seg.dst_port, p.src, seg.src_port, &o);
        self.send(alloc::vec![f]);
    }

    /// A SYN to a port listened on (TASK-184): a connection in SYN-RECEIVED
    /// while the listener's backlog and its client's connections have room,
    /// else dropped (the peer tries again). True when a listener had the port.
    #[cfg(feature = "flavour-e2e")]
    fn syn_to_listener(&mut self, e: &Eth, p: &Ip, seg: &Tcp, t: u64) -> bool {
        let Some(l) = self.listeners.iter().find(|l| l.port == seg.dst_port) else { return false };
        if seg.flags & (flag::SYN | flag::ACK | flag::RST) != flag::SYN {
            return false; // anything else for no connection: a RST, as always
        }
        let (client, backlog) = (l.client, l.backlog);
        let waiting = self.sockets.iter().filter(|s| s.waiting && s.local_port == seg.dst_port).count();
        let held = self.sockets.iter().filter(|s| s.client == client && !s.closed).count();
        if waiting >= backlog || held >= SOCKETS_PER_CLIENT {
            return true;
        }
        let iss = tcp::isn(&self.isn_key, &p.dst, seg.dst_port, &p.src, seg.src_port, t);
        self.next_socket = self.next_socket.wrapping_add(1).max(1);
        self.sockets.push(Socket {
            id: self.next_socket,
            client,
            local_port: seg.dst_port,
            to: p.src,
            port: seg.src_port,
            hop: p.src,
            mac: Some(e.src),
            arp_at: t,
            tcb: Tcb::accept(iss, MSS4, seg, t),
            connecting: None,
            reading: None,
            writing: None,
            closed: false,
            dns: None,
            waiting: true,
        });
        true
    }

    /// A port listened on for client `client` (TASK-184).
    #[cfg(feature = "flavour-e2e")]
    fn listen(&mut self, client: usize, port: u16, backlog: u16) -> Result<(), (FErr, &'static str)> {
        let who = self.names.get(client).map_or("", String::as_str);
        if !svc_netstack::grants::may_listen(who, true) {
            return Err((FErr::AccessDenied, "only the test clients listen"));
        }
        if port == 0 || backlog == 0 {
            return Err((FErr::InvalidArgs, "a port and a backlog of 1 to 8"));
        }
        if self.listeners.iter().any(|l| l.port == port) {
            return Err((FErr::Busy, "port taken"));
        }
        self.listeners.push(Listener { client, port, backlog: usize::from(backlog).min(BACKLOG), accepting: None });
        Ok(())
    }

    /// An ACCEPT on client `client`'s listener: answered by `accepting`.
    #[cfg(feature = "flavour-e2e")]
    fn accept(&mut self, client: usize, owed: Owed, port: u16, timeout_ms: u32) -> Result<(), (FErr, &'static str)> {
        let l = self
            .listeners
            .iter_mut()
            .find(|l| l.client == client && l.port == port)
            .ok_or((FErr::NotFound, "not listening there"))?;
        if l.accepting.is_some() {
            return Err((FErr::Busy, "an accept is waiting"));
        }
        l.accepting = Some((owed, now() + u64::from(timeout_ms.min(60_000)) * 1_000_000));
        Ok(())
    }

    /// Each listener's ACCEPT: the first connection waiting that's open, or
    /// "none came" at its deadline.
    #[cfg(feature = "flavour-e2e")]
    fn accepting(&mut self, clients: &[Channel], t: u64) {
        for l in &mut self.listeners {
            let Some((o, deadline)) = l.accepting else { continue };
            let ch = clients.get(l.client);
            let open = self.sockets.iter_mut().find(|s| {
                s.waiting && s.local_port == l.port && !matches!(s.tcb.state, TcpState::SynReceived | TcpState::Closed)
            });
            if let Some(s) = open {
                s.waiting = false;
                answer(ch, o, &nt::Accepted { id: s.id, from: s.to, from_port: s.port });
                l.accepting = None;
            } else if t >= deadline {
                refuse(ch, o, FErr::TimedOut, "none came");
                l.accepting = None;
            }
        }
    }

    /// Each connection: its neighbour found, its segments sent, its timers
    /// run, and the replies its client is owed.
    fn tcp_due(&mut self, clients: &[Channel]) {
        let t = now();
        let src = self.ours().map(|o| o.0);
        let mut i = 0;
        while i < self.sockets.len() {
            if let Some(src) = src {
                if self.sockets[i].mac.is_none() {
                    let hop = self.sockets[i].hop;
                    match self.arp.find(hop, t) {
                        Some(m) => self.sockets[i].mac = Some(m),
                        None if t >= self.sockets[i].arp_at => {
                            self.sockets[i].arp_at = t + ARP_RETRY;
                            self.arp_request(src, hop);
                        }
                        None => {}
                    }
                }
                if let Some(mac) = self.sockets[i].mac {
                    let segs = self.sockets[i].tcb.poll(t);
                    let (lp, to, port) = (self.sockets[i].local_port, self.sockets[i].to, self.sockets[i].port);
                    let frames = segs.iter().map(|o| self.tcp_frame(mac, src, lp, to, port, o)).collect();
                    self.send_frames(frames);
                }
            }
            if self.owed(i, clients, t) {
                i += 1;
            }
        }
        #[cfg(feature = "flavour-e2e")]
        self.accepting(clients, t);
    }

    /// The resolver's queries: UDP to the server (through its neighbour, or
    /// dropped till ARP or NDP finds it: the resolver asks again), or a connection.
    fn dns_send(&mut self, sends: Vec<resolver::Send>) {
        let v4 = self.ours();
        for q in sends {
            match q {
                resolver::Send::Udp { server: Server::V6(server), port, bytes } => self.dns_send6(server, port, &bytes),
                resolver::Send::Udp { server: Server::V4(server), port, bytes } => {
                    let Some((src, prefix, router)) = v4 else { continue };
                    let Some(hop) = next_hop(server, src, prefix, router) else { continue };
                    match self.arp.find(hop, now()) {
                        Some(mac) => {
                            let d = fnet::udp(src, server, port, fnet::dns::PORT, &bytes);
                            self.send_ip(mac, src, server, fnet::PROTO_UDP, &d);
                        }
                        None => self.arp_request(src, hop),
                    }
                }
                resolver::Send::Tcp { id, server, bytes } => {
                    let Some((src, prefix, router)) = v4 else { continue };
                    let Some(hop) = next_hop(server, src, prefix, router) else { continue };
                    let mut framed = (bytes.len() as u16).to_be_bytes().to_vec();
                    framed.extend_from_slice(&bytes);
                    let owed = Owed { txid: 0, ordinal: 0 };
                    if self.connect(usize::MAX, owed, server, fnet::dns::PORT, hop, 5_000).is_ok()
                        && let Some(s) = self.sockets.last_mut()
                    {
                        s.dns = Some((id, framed, Vec::new()));
                    }
                }
            }
        }
    }

    /// Finished lookups, answered to the clients waiting on them.
    fn resolved(&mut self, done: Vec<(u32, Answer)>, clients: &[Channel]) {
        for (id, a) in done {
            let Some(k) = self.resolving.iter().position(|r| r.0 == id) else { continue };
            let (_, client, o) = self.resolving.remove(k);
            let ch = clients.get(client);
            match a {
                Answer::Found(addresses) => answer(ch, o, &nt::Resolved { addresses }),
                Answer::NoSuchName => refuse(ch, o, FErr::NotFound, "no such name"),
                Answer::NoData => refuse(ch, o, FErr::NotFound, "no address"),
                Answer::NoAnswer => refuse(ch, o, FErr::TimedOut, "no answer"),
                Answer::NoServer => refuse(ch, o, FErr::BadState, "no DNS server"),
                Answer::Failed => refuse(ch, o, FErr::Integrity, "a bad answer"),
            }
        }
    }

    /// A DNS connection: the query once open, the answer when whole; false once it's gone.
    fn dns_socket(&mut self, i: usize, clients: &[Channel], t: u64) -> bool {
        let s = &mut self.sockets[i];
        let Some((id, query, got)) = &mut s.dns else { return true };
        let id = *id;
        if !query.is_empty() && matches!(s.tcb.state, TcpState::Established) {
            let n = s.tcb.write(query);
            query.drain(..n);
        }
        got.extend(s.tcb.read(64 * 1024));
        let whole = got.len() >= 2 && got.len() >= 2 + u16::from_be_bytes([got[0], got[1]]) as usize;
        let failed = s.tcb.state == TcpState::Closed
            || s.connecting.is_some_and(|(_, d)| t >= d && s.tcb.state == TcpState::SynSent);
        if !whole && !failed {
            return true;
        }
        let answer = whole.then(|| got[2..2 + u16::from_be_bytes([got[0], got[1]]) as usize].to_vec());
        s.tcb.close();
        s.dns = None;
        s.connecting = None;
        s.closed = true;
        let mut sends = Vec::new();
        let done = self.resolver.tcp(id, answer.as_deref(), t, self.ids.draw(), &mut sends);
        self.dns_send(sends);
        self.resolved(done, clients);
        true
    }

    /// The replies connection `i` owes its client; false once it's gone.
    fn owed(&mut self, i: usize, clients: &[Channel], t: u64) -> bool {
        if self.sockets[i].dns.is_some() {
            self.dns_socket(i, clients, t);
        }
        let s = &mut self.sockets[i];
        let ch = clients.get(s.client);
        let ended = s.tcb.ended;
        if let Some((o, deadline)) = s.connecting {
            match s.tcb.state {
                TcpState::SynSent if t < deadline => {}
                TcpState::SynSent | TcpState::Closed => {
                    let (e, why) = match ended {
                        Some(Ended::Refused) => (FErr::NotFound, "refused"),
                        Some(Ended::Reset) => (FErr::PeerClosed, "reset"),
                        _ => (FErr::TimedOut, "no answer"),
                    };
                    s.tcb.abort();
                    s.connecting = None;
                    s.closed = true;
                    refuse(ch, o, e, why);
                }
                _ => {
                    s.connecting = None;
                    answer(ch, o, &nt::Socket { id: s.id });
                }
            }
        }
        if let Some((o, data, taken)) = &mut s.writing {
            *taken += s.tcb.write(&data[*taken..]);
            let open = matches!(s.tcb.state, TcpState::SynSent | TcpState::Established | TcpState::CloseWait);
            if *taken == data.len() {
                answer(ch, *o, &nt::Sent { taken: *taken as u32 });
                s.writing = None;
            } else if !open {
                refuse(ch, *o, FErr::PeerClosed, if ended == Some(Ended::Reset) { "reset" } else { "closed" });
                s.writing = None;
            }
        }
        if let Some((o, max, deadline)) = s.reading {
            if s.tcb.readable() > 0 {
                answer(ch, o, &nt::Received { data: s.tcb.read(max), end: false });
                s.reading = None;
            } else if ended == Some(Ended::Reset) {
                refuse(ch, o, FErr::PeerClosed, "reset");
                s.reading = None;
            } else if s.tcb.at_end() || s.tcb.state == TcpState::Closed {
                answer(ch, o, &nt::Received { data: Vec::new(), end: true });
                s.reading = None;
            } else if t >= deadline {
                answer(ch, o, &nt::Received { data: Vec::new(), end: false });
                s.reading = None;
            }
        }
        // Closed by its client, or ended before its listener's client took it.
        let gone = (s.closed || s.waiting)
            && s.tcb.state == TcpState::Closed
            && s.connecting.is_none()
            && s.reading.is_none()
            && s.writing.is_none();
        if gone {
            self.sockets.remove(i);
        }
        !gone
    }

    /// A connection asked for: started once its next hop's MAC is known.
    fn connect(
        &mut self,
        client: usize,
        owed: Owed,
        to: Ipv4,
        port: u16,
        hop: Ipv4,
        timeout_ms: u32,
    ) -> Result<(), (FErr, &'static str)> {
        // The ones it holds: a connection it has closed (in FIN-WAIT or
        // TIME-WAIT for a while yet) isn't one (TASK-185, @net.limits).
        let held = self.sockets.iter().filter(|s| s.client == client && !s.closed).count();
        if client != usize::MAX && held >= SOCKETS_PER_CLIENT {
            return Err((FErr::Busy, "too many connections"));
        }
        let (src, _, _) = self.ours().ok_or((FErr::BadState, "no address yet"))?;
        let local_port = (0..EPHEMERAL.len())
            .find_map(|_| {
                let p = self.next_port;
                self.next_port = if p == *EPHEMERAL.end() { *EPHEMERAL.start() } else { p + 1 };
                (!self.sockets.iter().any(|s| s.local_port == p)).then_some(p)
            })
            .ok_or((FErr::Busy, "no free port"))?;
        let t = now();
        let iss = tcp::isn(&self.isn_key, &src, local_port, &to, port, t);
        self.next_socket = self.next_socket.wrapping_add(1).max(1);
        let wait = timeout_ms.clamp(100, 120_000) as u64 * 1_000_000;
        self.sockets.push(Socket {
            id: self.next_socket,
            client,
            local_port,
            to,
            port,
            hop,
            mac: None,
            arp_at: t,
            tcb: Tcb::connect(iss, MSS4, t),
            connecting: Some((owed, t + wait)),
            reading: None,
            writing: None,
            closed: false,
            dns: None,
            waiting: false,
        });
        Ok(())
    }

    /// Client `i`'s connection `id`.
    fn socket(&mut self, client: usize, id: u32) -> Result<&mut Socket, (FErr, &'static str)> {
        self.sockets.iter_mut().find(|s| s.client == client && s.id == id).ok_or((FErr::NotFound, "no such connection"))
    }

    /// Client `c` is gone: its connections reset, its pings dropped, the later clients' indices moved down.
    fn drop_client(&mut self, c: usize) {
        if c < self.names.len() {
            self.names.remove(c);
        }
        let src = self.ours().map(|o| o.0);
        let mut i = 0;
        while i < self.sockets.len() {
            if self.sockets[i].client == c {
                let mut s = self.sockets.remove(i);
                if let (Some(rst), Some(mac), Some(src)) = (s.tcb.abort(), s.mac, src) {
                    let f = self.tcp_frame(mac, src, s.local_port, s.to, s.port, &rst);
                    self.send(alloc::vec![f]);
                }
                continue;
            }
            if self.sockets[i].client > c && self.sockets[i].client != usize::MAX {
                self.sockets[i].client -= 1;
            }
            i += 1;
        }
        self.pending.retain(|q| q.client != c);
        self.resolving.retain(|r| r.1 != c);
        #[cfg(feature = "flavour-e2e")]
        {
            self.listeners.retain(|l| l.client != c);
            for l in &mut self.listeners {
                if l.client > c {
                    l.client -= 1;
                }
            }
        }
        for r in &mut self.resolving {
            if r.1 > c {
                r.1 -= 1;
            }
        }
        for q in &mut self.pending {
            if q.client > c {
                q.client -= 1;
            }
        }
    }

    /// The resolver's servers (the lease's), its retries, and its deadlines.
    /// A query to an IPv6 server (RDNSS's, TASK-176): to it on the link, else
    /// through the router; dropped till NDP finds the neighbour.
    fn dns_send6(&mut self, server: Ipv6, port: u16, bytes: &[u8]) {
        let Some(src) = self.slaac.source_for(&server) else { return };
        let hop = if self.slaac.on_link(&server) { Some(server) } else { self.slaac.router.map(|r| r.0) };
        let Some(hop) = hop else { return };
        let t = now();
        let mac = self.nd6.find(hop, t).or(self.slaac.router.filter(|r| r.0 == hop).map(|r| r.1));
        match mac {
            Some(mac) => {
                let d = v6::udp6(&src, &server, port, fnet::dns::PORT, bytes);
                let packet = v6::ip6(src, server, v6::NEXT_UDP, 64, &d);
                let frame = fnet::eth(mac, self.mac, v6::ETHERTYPE_IPV6, &packet);
                self.send(alloc::vec![frame]);
            }
            None => {
                let from = self.slaac.source_for(&hop).unwrap_or(src);
                self.ndp_request(from, hop);
            }
        }
    }

    fn dns_due(&mut self, clients: &[Channel]) {
        // DHCP's servers first, then RDNSS's (TASK-176).
        let v4 = self.dhcp.lease().map(|l| l.dns.clone()).unwrap_or_default();
        let servers: Vec<Server> =
            v4.into_iter().map(Server::V4).chain(self.slaac.dns.iter().copied().map(Server::V6)).collect();
        if self.resolver.servers != servers {
            self.resolver.servers = servers;
        }
        let mut sends = Vec::new();
        let done = self.resolver.due(now(), || self.ids.draw(), &mut sends);
        self.dns_send(sends);
        self.resolved(done, clients);
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

    /// An IPv6 packet: ICMPv6 (echo, and NDP's four messages), and the DNS
    /// answers of the servers RDNSS gave (TASK-176).
    fn ipv6(&mut self, e: &Eth, clients: &[Channel]) {
        let Ok(p) = Ip6::parse(e.payload) else { return };
        if p.next == v6::NEXT_UDP {
            if self.slaac.ours(&p.dst)
                && let Ok(u) = v6::udp6_parse(&p.src, &p.dst, p.payload)
                && u.src_port == fnet::dns::PORT
                && self.resolver.ports().any(|q| q == u.dst_port)
            {
                let mut sends = Vec::new();
                let done = self.resolver.udp(Server::V6(p.src), u.dst_port, u.data, now(), self.ids.draw(), &mut sends);
                self.dns_send(sends);
                self.resolved(done, clients);
            }
            return;
        }
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
        if let Some(w) = self.resolver.next_wake() {
            t = t.min(w);
        }
        let addressed = self.ours().is_some();
        for s in &self.sockets {
            // A connection waiting for its neighbour runs no timer of its own yet.
            match s.mac {
                Some(_) => t = s.tcb.next_wake().map_or(t, |w| t.min(w)),
                None if addressed => t = t.min(s.arp_at),
                None => {}
            }
            if let Some((_, d)) = s.connecting {
                t = t.min(d);
            }
            if let Some((_, _, d)) = s.reading {
                t = t.min(d);
            }
        }
        #[cfg(feature = "flavour-e2e")]
        for l in &self.listeners {
            if let Some((_, d)) = l.accepting {
                t = t.min(d);
            }
        }
        t
    }

    /// Each network's counts, as its driver keeps them (TASK-186).
    fn traffic(&mut self) -> Result<nt::Traffic, (FErr, &'static str)> {
        let wait = Instant::after(Duration::from_secs(2));
        let (c, _) = self
            .dev
            .call::<_, fwire::proto::netdev::Stats>(fwire::proto::netdev::STATS, &(), Vec::new(), wait)
            .map_err(|_| (FErr::BadState, "the network driver didn't answer"))?;
        Ok(nt::Traffic {
            list: alloc::vec![nt::Counts {
                name: String::from("eth0"),
                rx_frames: c.rx_frames,
                rx_bytes: c.rx_bytes,
                rx_dropped: c.rx_dropped,
                tx_frames: c.tx_frames,
                tx_bytes: c.tx_bytes,
                tx_dropped: c.tx_dropped,
            }],
        })
    }

    /// Every connection this computer can make, and the TCP ones open now.
    fn connections(&self) -> nt::Connections {
        let grants = svc_netstack::grants::list(cfg!(feature = "flavour-e2e"))
            .map(|g| nt::Grant { who: String::from(g.who), what: String::from(g.what), when: String::from(g.when) })
            .collect();
        let open = self
            .sockets
            .iter()
            // Those a program holds: one it closed (FIN-WAIT, TIME-WAIT) isn't open.
            .filter(|k| !k.closed && k.tcb.state != svc_netstack::tcp::State::Closed)
            .map(|k| nt::Open {
                who: self.names.get(k.client).cloned().unwrap_or_else(|| String::from("?")),
                to: k.to,
                port: k.port,
                state: alloc::format!("{:?}", k.tcb.state),
            })
            .collect();
        nt::Connections { grants, open }
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
        if self.pending.iter().filter(|q| q.client == client).count() >= PINGS_PER_CLIENT {
            return Err((FErr::Busy, "too many pings waiting"));
        }
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
    let mut r = [0u8; 12 + 32 + 2 + 32];
    frandom::fill(&mut r).map_err(|_| String::from("no randomness (RDRAND/RDSEED refused)"))?;
    let xid = u32::from_le_bytes([r[0], r[1], r[2], r[3]]);
    let iid: [u8; 8] = r[4..12].try_into().map_err(|_| String::from("no randomness"))?;
    let isn_key: [u8; 32] = r[12..44].try_into().map_err(|_| String::from("no randomness"))?;
    let span = EPHEMERAL.len() as u16;
    let next_port = EPHEMERAL.start() + u16::from_le_bytes([r[44], r[45]]) % span;
    let ids: [u8; 32] = r[46..78].try_into().map_err(|_| String::from("no randomness"))?;
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
        sockets: Vec::new(),
        next_socket: 0,
        next_port,
        isn_key,
        ids: Ids::new(ids),
        resolver: Resolver::new(),
        resolving: Vec::new(),
        names: Vec::new(),
        #[cfg(feature = "flavour-e2e")]
        listeners: Vec::new(),
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
        s.tcp_due(&clients);
        s.dns_due(&clients);
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
                            s.names.push(inc.client);
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
                    s.drop_client(k - 2);
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
        let owed = Owed { txid: h.txid, ordinal: h.ordinal };
        let asked = match h.ordinal {
            nt::INTERFACES => Ok(Some(fwire::reply(h.txid, h.ordinal, &s.interfaces()))),
            nt::CONNECTIONS => Ok(Some(fwire::reply(h.txid, h.ordinal, &s.connections()))),
            nt::STATS => s.traffic().map(|t| Some(fwire::reply(h.txid, h.ordinal, &t))),
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
            nt::TCP_CONNECT => match (fwire::from_payload::<nt::Connect>(p), s.ours()) {
                (Err(_), _) => Err((FErr::InvalidArgs, "unreadable")),
                (Ok(_), None) => Err((FErr::BadState, "no address yet")),
                (Ok(c), Some((ours, prefix, router))) => match next_hop(c.to, ours, prefix, router) {
                    None => Err((FErr::NotFound, "no route")),
                    Some(hop) => s.connect(i, owed, c.to, c.port, hop, c.timeout_ms).map(|_| None),
                },
            },
            nt::RESOLVE => match fwire::from_payload::<nt::Resolve>(p) {
                Err(_) => Err((FErr::InvalidArgs, "unreadable")),
                Ok(q) if q.kind != fnet::dns::TYPE_A && q.kind != fnet::dns::TYPE_AAAA => {
                    Err((FErr::InvalidArgs, "kind is 1 (A) or 28 (AAAA)"))
                }
                Ok(_) if s.resolving.iter().filter(|r| r.1 == i).count() >= LOOKUPS_PER_CLIENT => {
                    Err((FErr::Busy, "too many lookups waiting"))
                }
                Ok(q) => {
                    let mut sends = Vec::new();
                    match s.resolver.lookup(&q.name, q.kind, now(), s.ids.draw(), &mut sends) {
                        // From the cache, or a name that can't be one: answered now.
                        Ok(Answer::Found(addresses)) => {
                            Ok(Some(fwire::reply(h.txid, h.ordinal, &nt::Resolved { addresses })))
                        }
                        Ok(Answer::NoSuchName) => Err((FErr::NotFound, "no such name")),
                        Ok(Answer::NoData) => Err((FErr::NotFound, "no address")),
                        Ok(Answer::NoAnswer) => Err((FErr::TimedOut, "no answer")),
                        Ok(Answer::NoServer) => Err((FErr::BadState, "no DNS server (the network gave none)")),
                        Ok(Answer::Failed) => Err((FErr::InvalidArgs, "not a name")),
                        Err(id) => {
                            s.resolving.push((id, i, owed));
                            s.dns_send(sends);
                            Ok(None)
                        }
                    }
                }
            },
            nt::TCP_SEND => match fwire::from_payload::<nt::Send>(p) {
                Err(_) => Err((FErr::InvalidArgs, "unreadable")),
                Ok(m) => s.socket(i, m.id).and_then(|so| match so.writing {
                    Some(_) => Err((FErr::Busy, "a send is waiting")),
                    None => {
                        so.writing = Some((owed, m.data, 0));
                        Ok(None)
                    }
                }),
            },
            nt::TCP_RECV => match fwire::from_payload::<nt::Recv>(p) {
                Err(_) => Err((FErr::InvalidArgs, "unreadable")),
                Ok(m) => s.socket(i, m.id).and_then(|so| match so.reading {
                    Some(_) => Err((FErr::Busy, "a receive is waiting")),
                    None => {
                        let wait = m.timeout_ms.min(60_000) as u64 * 1_000_000;
                        so.reading = Some((owed, (m.max as usize).clamp(1, 60 * 1024), now() + wait));
                        Ok(None)
                    }
                }),
            },
            #[cfg(feature = "flavour-e2e")]
            nt::TCP_LISTEN => match fwire::from_payload::<nt::Listen>(p) {
                Err(_) => Err((FErr::InvalidArgs, "unreadable")),
                Ok(l) => s.listen(i, l.port, l.backlog).map(|_| Some(fwire::reply(h.txid, h.ordinal, &()))),
            },
            #[cfg(feature = "flavour-e2e")]
            nt::TCP_ACCEPT => match fwire::from_payload::<nt::Accept>(p) {
                Err(_) => Err((FErr::InvalidArgs, "unreadable")),
                Ok(a) => s.accept(i, owed, a.port, a.timeout_ms).map(|_| None),
            },
            nt::TCP_CLOSE => match fwire::from_payload::<nt::Socket>(p) {
                Err(_) => Err((FErr::InvalidArgs, "unreadable")),
                Ok(m) => s.socket(i, m.id).map(|so| {
                    so.tcb.close();
                    so.closed = true;
                    Some(fwire::reply(h.txid, h.ordinal, &()))
                }),
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
