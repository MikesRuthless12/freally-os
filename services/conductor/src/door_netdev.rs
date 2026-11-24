// SPDX-License-Identifier: Apache-2.0
//! The test door's network device (e2e flavour only, TASK-172): before
//! netstack, the driver proved on its own.
//! - `netdev info` → `info <driver> mac=<mac> mtu=<n> link=up|down`;
//! - `netdev stats` → `stats rx=<frames>/<bytes> rx_dropped=<n> tx=<frames>/<bytes> tx_dropped=<n>`;
//! - `netdev arp <ip>` sends an ARP request for `<ip>` from 10.0.2.15 (QEMU's
//!   user network gives the guest that address) and waits 2 s for the
//!   answer: `reply <ip> is-at <mac>`, or `error: …`.

use crate::services::Conductor;
use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::signal;
use fstd::ipc::{Channel, Client};
use fstd::time::{Duration, Instant};
use fwire::proto::netdev as nd;

/// The guest's address on QEMU's user network.
const GUEST: [u8; 4] = [10, 0, 2, 15];

#[derive(Default)]
pub struct NetDoor {
    client: Option<Client>,
    events: Option<Channel>,
}

fn mac(m: &[u8; 6]) -> String {
    alloc::format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5])
}

fn ipv4(s: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut parts = s.split('.');
    for b in out.iter_mut() {
        *b = parts.next()?.parse().ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// An ARP request (RFC 826) for `target` from `from` at `me`, padded to
/// Ethernet's 60 bytes.
fn arp_request(me: [u8; 6], from: [u8; 4], target: [u8; 4]) -> Vec<u8> {
    let mut f = Vec::with_capacity(60);
    f.extend_from_slice(&[0xFF; 6]);
    f.extend_from_slice(&me);
    f.extend_from_slice(&[0x08, 0x06, 0, 1, 0x08, 0x00, 6, 4, 0, 1]);
    f.extend_from_slice(&me);
    f.extend_from_slice(&from);
    f.extend_from_slice(&[0; 6]);
    f.extend_from_slice(&target);
    f.resize(60, 0);
    f
}

/// `frame`'s sender, if it's an ARP reply about `target` addressed to `me`.
fn arp_reply(frame: &[u8], me: [u8; 6], target: [u8; 4]) -> Option<[u8; 6]> {
    (frame.len() >= 42
        && frame[0..6] == me
        && frame[12..14] == [0x08, 0x06]
        && frame[20..22] == [0, 2]
        && frame[28..32] == target)
        .then(|| frame[22..28].try_into().ok())
        .flatten()
}

impl NetDoor {
    pub fn run(&mut self, c: &mut Conductor, arg: &str) -> Vec<String> {
        if self.client.is_none() {
            self.client =
                ["drv-virtio-net", "drv-e1000"].iter().find_map(|d| c.open_connection(d, "conductor")).map(Client::new);
            self.events = None;
        }
        let r = match arg.split_once(' ').unwrap_or((arg, "")) {
            ("info", _) => self.call::<_, nd::Info>(nd::INFO, &()).map(|i| {
                alloc::vec![alloc::format!(
                    "info {} mac={} mtu={} link={}",
                    i.driver,
                    mac(&i.mac),
                    i.mtu,
                    if i.link { "up" } else { "down" }
                )]
            }),
            ("stats", _) => self.call::<_, nd::Stats>(nd::STATS, &()).map(|s| {
                alloc::vec![alloc::format!(
                    "stats rx={}/{} rx_dropped={} tx={}/{} tx_dropped={}",
                    s.rx_frames,
                    s.rx_bytes,
                    s.rx_dropped,
                    s.tx_frames,
                    s.tx_bytes,
                    s.tx_dropped
                )]
            }),
            ("arp", ip) => match ipv4(ip.trim()) {
                Some(target) => {
                    self.arp(target).map(|m| alloc::vec![alloc::format!("reply {} is-at {}", ip.trim(), mac(&m))])
                }
                None => Err(alloc::format!("{ip:?} isn't an IPv4 address")),
            },
            _ => Err(String::from("netdev info|stats|arp <ip>")),
        };
        r.unwrap_or_else(|e| {
            self.client = None;
            alloc::vec![alloc::format!("error: {e}")]
        })
    }

    fn call<Q: fwire::Wire, R: fwire::Wire>(&mut self, ordinal: u32, req: &Q) -> Result<R, String> {
        let cl = self.client.as_mut().ok_or("the network device isn't up")?;
        cl.call::<_, R>(ordinal, req, Vec::new(), Instant::after(Duration::from_secs(2)))
            .map(|(r, _)| r)
            .map_err(|e| alloc::format!("{e:?}"))
    }

    fn arp(&mut self, target: [u8; 4]) -> Result<[u8; 6], String> {
        let info: nd::Info = self.call(nd::INFO, &())?;
        if self.events.is_none() {
            let cl = self.client.as_mut().ok_or("the network device isn't up")?;
            let (_, hs) = cl
                .call::<_, nd::Subscribed>(nd::SUBSCRIBE, &(), Vec::new(), Instant::after(Duration::from_secs(2)))
                .map_err(|e| alloc::format!("{e:?}"))?;
            self.events = Some(Channel(hs.into_iter().next().ok_or("no events channel")?));
        }
        let sent: nd::Sent =
            self.call(nd::SEND, &nd::Frames { frames: alloc::vec![arp_request(info.mac, GUEST, target)] })?;
        if sent.sent != 1 {
            return Err(String::from("the device took no frame"));
        }
        let ev = self.events.as_ref().ok_or("no events channel")?;
        let until = Instant::after(Duration::from_secs(2));
        loop {
            if fstd::sys::wait_one(ev.0.raw(), signal::READABLE, until).is_err() {
                return Err(String::from("no ARP reply in 2 s"));
            }
            while let Ok(m) = ev.try_recv() {
                let Ok((h, p)) = fwire::parse(&m.bytes) else { continue };
                if h.ordinal != nd::EV_FRAMES {
                    continue;
                }
                let Ok(frames) = fwire::from_payload::<nd::Frames>(p) else { continue };
                if let Some(m) = frames.frames.iter().find_map(|f| arp_reply(f, info.mac, target)) {
                    return Ok(m);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_arp_request_and_its_reply() {
        let me = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
        let f = arp_request(me, GUEST, [10, 0, 2, 2]);
        assert_eq!(f.len(), 60);
        assert_eq!(&f[12..22], &[0x08, 0x06, 0, 1, 0x08, 0x00, 6, 4, 0, 1]);
        assert_eq!(&f[38..42], &[10, 0, 2, 2]);
        // The gateway's answer, as QEMU's user network sends it.
        let gw = [0x52, 0x55, 10, 0, 2, 2];
        let mut r = Vec::new();
        r.extend_from_slice(&me);
        r.extend_from_slice(&gw);
        r.extend_from_slice(&[0x08, 0x06, 0, 1, 0x08, 0x00, 6, 4, 0, 2]);
        r.extend_from_slice(&gw);
        r.extend_from_slice(&[10, 0, 2, 2]);
        r.extend_from_slice(&me);
        r.extend_from_slice(&GUEST);
        assert_eq!(arp_reply(&r, me, [10, 0, 2, 2]), Some(gw));
        assert_eq!(arp_reply(&r, me, [10, 0, 2, 3]), None, "about another address");
        assert_eq!(arp_reply(&f, me, [10, 0, 2, 2]), None, "a request isn't a reply");
        assert_eq!(arp_reply(&r[..41], me, [10, 0, 2, 2]), None, "too short");
    }

    #[test]
    fn addresses_are_read_whole() {
        assert_eq!(ipv4("10.0.2.2"), Some([10, 0, 2, 2]));
        assert_eq!(ipv4("10.0.2"), None);
        assert_eq!(ipv4("10.0.2.2.1"), None);
        assert_eq!(ipv4("10.0.2.256"), None);
    }
}
