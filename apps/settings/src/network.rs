// SPDX-License-Identifier: Apache-2.0
//! Settings ▸ Network (TASK-183, FR-060): each network (its driver, MAC,
//! link, addresses, router and DNS servers), then every connection this
//! computer can make, each with what it is and when (netstack's own list,
//! the one it checks every program against), then the connections open now
//! and whose. Read from netstack each time the page opens; nothing on it
//! moves while it's shown.

use alloc::string::String;
use alloc::vec::Vec;
use fwire::proto::net as nt;

#[derive(Clone, Debug, Default)]
pub struct View {
    /// netstack's answers, or why there are none.
    pub ifaces: Option<Result<Vec<nt::Interface>, String>>,
    pub conns: Option<Result<nt::Connections, String>>,
}

fn dotted(a: [u8; 4]) -> String {
    alloc::format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3])
}

fn mac(m: [u8; 6]) -> String {
    alloc::format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5])
}

fn joined(v: impl Iterator<Item = String>) -> String {
    let v: Vec<String> = v.collect();
    if v.is_empty() { String::from("none") } else { v.join(", ") }
}

impl View {
    /// The page's lines in order: (test id, words); headings have none.
    pub fn lines(&self) -> Vec<(Option<String>, String)> {
        let mut v = Vec::new();
        let id = |kind: &str, i: usize| Some(alloc::format!("settings-network-{kind}-{i}"));
        match &self.ifaces {
            None => {}
            Some(Err(why)) => v.push((id("iface", 0), why.clone())),
            Some(Ok(list)) if list.is_empty() => {
                v.push((id("iface", 0), String::from("There's no network on this computer.")))
            }
            Some(Ok(list)) => {
                for (k, n) in list.iter().enumerate() {
                    v.push((
                        id("iface", k),
                        alloc::format!(
                            "{} · {} · {} · {}",
                            n.name,
                            n.driver,
                            mac(n.mac),
                            if n.link { "connected" } else { "not connected" }
                        ),
                    ));
                    v.push((
                        id("ipv4", k),
                        match n.ipv4 {
                            Some((a, p)) => alloc::format!(
                                "IPv4 {}/{p} · router {} · DNS {}",
                                dotted(a),
                                n.router.map_or(String::from("none"), dotted),
                                joined(n.dns.iter().map(|d| dotted(*d)))
                            ),
                            None => String::from("IPv4: no address yet"),
                        },
                    ));
                    if !n.ipv6.is_empty() {
                        v.push((
                            id("ipv6", k),
                            alloc::format!(
                                "IPv6 {} · router {} · DNS {}",
                                joined(n.ipv6.iter().map(|(a, p)| alloc::format!("{}/{p}", fnet::ipv6::text(a)))),
                                n.router6.map_or(String::from("none"), |r| fnet::ipv6::text(&r)),
                                joined(n.dns6.iter().map(fnet::ipv6::text))
                            ),
                        ));
                    }
                }
            }
        }
        match &self.conns {
            None => {}
            Some(Err(why)) => v.push((id("grant", 0), why.clone())),
            Some(Ok(c)) => {
                v.push((None, String::from("Every connection this computer can make")));
                for (k, g) in c.grants.iter().enumerate() {
                    v.push((id("grant", k), alloc::format!("{}: {}; {}.", g.who, g.what, g.when)));
                }
                v.push((None, String::from("Open now")));
                if c.open.is_empty() {
                    v.push((id("open", 0), String::from("No connection is open.")));
                }
                for (k, o) in c.open.iter().enumerate() {
                    v.push((
                        id("open", k),
                        alloc::format!("{}: TCP to {}:{} ({})", o.who, dotted(o.to), o.port, o.state),
                    ));
                }
            }
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface() -> nt::Interface {
        nt::Interface {
            name: String::from("eth0"),
            driver: String::from("virtio-net"),
            mac: [0x52, 0x54, 0, 0x12, 0x34, 0x56],
            link: true,
            ipv4: Some(([10, 0, 2, 15], 24)),
            router: Some([10, 0, 2, 2]),
            dns: alloc::vec![[10, 0, 2, 3]],
            lease_s: 86_400,
            ipv6: alloc::vec![([0xfe, 0xc0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x15], 64)],
            router6: None,
            dns6: Vec::new(),
        }
    }

    #[test]
    fn each_network_its_addresses_then_every_grant_then_what_is_open() {
        let v = View {
            ifaces: Some(Ok(alloc::vec![iface()])),
            conns: Some(Ok(nt::Connections {
                grants: alloc::vec![nt::Grant {
                    who: String::from("terminal"),
                    what: String::from("what you type"),
                    when: String::from("only when you run them"),
                }],
                open: alloc::vec![nt::Open {
                    who: String::from("updater"),
                    to: [10, 77, 3, 1],
                    port: 443,
                    state: String::from("Established"),
                }],
            })),
        };
        let words: Vec<String> = v.lines().into_iter().map(|l| l.1).collect();
        assert_eq!(
            words,
            [
                "eth0 · virtio-net · 52:54:00:12:34:56 · connected",
                "IPv4 10.0.2.15/24 · router 10.0.2.2 · DNS 10.0.2.3",
                "IPv6 fec0::15/64 · router none · DNS none",
                "Every connection this computer can make",
                "terminal: what you type; only when you run them.",
                "Open now",
                "updater: TCP to 10.77.3.1:443 (Established)",
            ]
        );
        assert_eq!(v.lines()[4].0.as_deref(), Some("settings-network-grant-0"));
    }

    #[test]
    fn no_network_and_nothing_open_say_so() {
        let v = View {
            ifaces: Some(Ok(Vec::new())),
            conns: Some(Ok(nt::Connections { grants: Vec::new(), open: Vec::new() })),
        };
        let words: Vec<String> = v.lines().into_iter().map(|l| l.1).collect();
        assert_eq!(words[0], "There's no network on this computer.");
        assert_eq!(words.last().map(String::as_str), Some("No connection is open."));
    }
}
