// SPDX-License-Identifier: Apache-2.0
//! netstack's limits (TASK-185): every table a peer on the network or a
//! client of netstack can grow, and the most it holds, in one place.
//!
//! From the network: the neighbour caches (ARP's for IPv4, NDP's for IPv6)
//! keep `NEIGHBOURS` entries for `NEIGHBOUR_KEEP_S`, the oldest making room;
//! the DNS answers cached are `DNS_CACHE`, the oldest making room; no
//! fragment is reassembled (fnet refuses them: nothing to grow). From a
//! client: `SOCKETS_PER_CLIENT` TCP connections at once, each with
//! `TCP_BUFFER` bytes buffered each way.

/// Entries in each neighbour cache, and how long one is kept.
pub const NEIGHBOURS: usize = crate::ArpCache::MAX;
pub const NEIGHBOUR_KEEP_S: u64 = crate::ArpCache::KEEP / 1_000_000_000;
/// DNS answers cached.
pub const DNS_CACHE: usize = 128;
/// TCP connections one client may hold at once.
pub const SOCKETS_PER_CLIENT: usize = 16;
/// Bytes a TCP connection buffers each way.
pub const TCP_BUFFER: usize = crate::tcp::BUFFER;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ArpCache;

    #[test]
    fn a_neighbour_cache_never_holds_more_than_its_limit() {
        let mut c = ArpCache::default();
        for i in 0..1000u32 {
            let ip = i.to_be_bytes();
            c.learn(ip, [2, 0, 0, 0, (i >> 8) as u8, i as u8], u64::from(i));
            assert!(c.len() <= NEIGHBOURS, "{} entries after {i}", c.len());
        }
        assert_eq!(c.len(), NEIGHBOURS);
        assert!(c.find(999u32.to_be_bytes(), 999).is_some(), "the newest is kept");
        assert!(c.find(0u32.to_be_bytes(), 999).is_none(), "the oldest made room");
        // And an entry ages out.
        let keep = NEIGHBOUR_KEEP_S * 1_000_000_000;
        assert!(c.find(999u32.to_be_bytes(), 999 + keep).is_none());
    }

    #[test]
    fn no_fragment_is_reassembled() {
        let (a, b) = ([10, 0, 2, 15], [10, 0, 2, 2]);
        for flags in [0x2000u16, 0x0001, 0x2010] {
            let mut p = fnet::ip(a, b, fnet::PROTO_UDP, 7, &fnet::udp(a, b, 1, 2, b"piece"));
            p[6..8].copy_from_slice(&flags.to_be_bytes());
            p[10..12].fill(0);
            let sum = fnet::checksum(&p[..20], 0);
            p[10..12].copy_from_slice(&sum.to_be_bytes());
            assert_eq!(fnet::Ip::parse(&p).map(|_| ()), Err(fnet::Bad::Fragment), "flags {flags:#x}");
        }
    }
}
