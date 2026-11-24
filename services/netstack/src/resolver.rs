// SPDX-License-Identifier: Apache-2.0
//! The stub resolver (TASK-176), tested on the host: a name's A or AAAA
//! records from the servers DHCP (and RDNSS) gave, over UDP from a random
//! port with a random id, again over TCP when the answer was truncated;
//! CNAME chains followed (at most 8); answers cached by their TTL (at most a
//! day), a name that doesn't exist for a minute. It knows no sockets: it
//! says what to send, and is told what came back. Times are nanoseconds.

use alloc::string::String;
use alloc::vec::Vec;

use fnet::Ipv4;
use fnet::dns::{self, Found};

const SECOND: u64 = 1_000_000_000;
/// A UDP query's wait before it goes again, doubling: 1, 2 s, then the next server.
const FIRST_WAIT: u64 = SECOND;
const TRIES_PER_SERVER: u32 = 2;
/// A lookup given up after this, whatever is left.
const DEADLINE: u64 = 10 * SECOND;
const MAX_TTL: u32 = 86_400;
const NEGATIVE_TTL: u64 = 60 * SECOND;
const CACHE: usize = crate::limits::DNS_CACHE;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// The addresses (4 or 16 bytes each).
    Found(Vec<Vec<u8>>),
    NoSuchName,
    /// The name has no address of this kind.
    NoData,
    /// No server answered in time.
    NoAnswer,
    /// The network gave no DNS server.
    NoServer,
    /// A server's answer was malformed, or a chain too long.
    Failed,
}

/// What the resolver wants sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Send {
    /// A UDP query to `server`:53 from `port`.
    Udp { server: Ipv4, port: u16, bytes: Vec<u8> },
    /// A TCP query to `server`:53 (the message, its 2-byte length added by the sender), for lookup `id`.
    Tcp { id: u32, server: Ipv4, bytes: Vec<u8> },
}

struct Lookup {
    id: u32,
    name: String,
    qtype: u16,
    /// The name being asked now (the name itself, or an alias of it).
    asking: String,
    aliases: usize,
    ttl: u32,
    txid: u16,
    port: u16,
    server: usize,
    tries: u32,
    wait: u64,
    next: u64,
    deadline: u64,
    tcp: bool,
}

struct Cached {
    name: String,
    qtype: u16,
    answer: Answer,
    until: u64,
}

pub struct Resolver {
    pub servers: Vec<Ipv4>,
    lookups: Vec<Lookup>,
    cache: Vec<Cached>,
    next_id: u32,
}

impl Default for Resolver {
    fn default() -> Self {
        Resolver::new()
    }
}

impl Resolver {
    pub fn new() -> Resolver {
        Resolver { servers: Vec::new(), lookups: Vec::new(), cache: Vec::new(), next_id: 0 }
    }

    /// The answers cached now.
    pub fn cached(&self) -> usize {
        self.cache.len()
    }

    /// Starts a lookup: Ok(answer) at once from the cache (or for a name that
    /// can't be one), else Err(the lookup's id) with its first query in `out`.
    /// `random` is fresh randomness: the query's id and source port.
    pub fn lookup(
        &mut self,
        name: &str,
        qtype: u16,
        now: u64,
        random: [u8; 4],
        out: &mut Vec<Send>,
    ) -> Result<Answer, u32> {
        let Some(name) = dns::canonical(name) else { return Ok(Answer::Failed) };
        self.cache.retain(|c| c.until > now);
        if let Some(c) = self.cache.iter().find(|c| c.name == name && c.qtype == qtype) {
            return Ok(c.answer.clone());
        }
        if self.servers.is_empty() {
            return Ok(Answer::NoServer);
        }
        self.next_id = self.next_id.wrapping_add(1);
        let id = self.next_id;
        let mut l = Lookup {
            id,
            asking: name.clone(),
            name,
            qtype,
            aliases: 0,
            ttl: MAX_TTL,
            txid: 0,
            port: 0,
            server: 0,
            tries: 0,
            wait: FIRST_WAIT,
            next: now,
            deadline: now + DEADLINE,
            tcp: false,
        };
        Resolver::ask(&self.servers, &mut l, now, random, out);
        self.lookups.push(l);
        Err(id)
    }

    /// The lookup's next UDP query: a fresh id and port each time (RFC 5452).
    fn ask(servers: &[Ipv4], l: &mut Lookup, now: u64, random: [u8; 4], out: &mut Vec<Send>) {
        l.txid = u16::from_be_bytes([random[0], random[1]]);
        // An ephemeral port, 49152–65535.
        l.port = 49152 + u16::from_be_bytes([random[2], random[3]]) % 16384;
        l.tcp = false;
        l.tries += 1;
        l.next = now + l.wait;
        if let Some(bytes) = dns::query(l.txid, &l.asking, l.qtype) {
            out.push(Send::Udp { server: servers[l.server], port: l.port, bytes });
        }
    }

    /// The UDP ports lookups are waiting on (netstack takes datagrams to them).
    pub fn ports(&self) -> impl Iterator<Item = u16> + '_ {
        self.lookups.iter().filter(|l| !l.tcp).map(|l| l.port)
    }

    /// A UDP datagram from `from`:53 to our `port`. Finished lookups come back as (id, answer).
    pub fn udp(
        &mut self,
        from: Ipv4,
        port: u16,
        bytes: &[u8],
        now: u64,
        random: [u8; 4],
        out: &mut Vec<Send>,
    ) -> Vec<(u32, Answer)> {
        let Some(i) =
            self.lookups.iter().position(|l| !l.tcp && l.port == port && self.servers.get(l.server) == Some(&from))
        else {
            return Vec::new();
        };
        let Ok(r) = dns::parse(bytes) else { return Vec::new() };
        // Not ours (the id or the question differs): ignored, as a spoof would be.
        let l = &self.lookups[i];
        if r.id != l.txid || r.question != (l.asking.clone(), l.qtype) {
            return Vec::new();
        }
        if r.truncated {
            let l = &mut self.lookups[i];
            l.tcp = true;
            l.next = l.deadline;
            if let Some(q) = dns::query(l.txid, &l.asking, l.qtype) {
                out.push(Send::Tcp { id: l.id, server: from, bytes: q });
            }
            return Vec::new();
        }
        self.answered(i, &r, now, random, out)
    }

    /// The whole answer to lookup `id`'s TCP query (its 2-byte length taken off),
    /// or None when the connection failed.
    pub fn tcp(
        &mut self,
        id: u32,
        bytes: Option<&[u8]>,
        now: u64,
        random: [u8; 4],
        out: &mut Vec<Send>,
    ) -> Vec<(u32, Answer)> {
        let Some(i) = self.lookups.iter().position(|l| l.id == id && l.tcp) else { return Vec::new() };
        let r = bytes.map(dns::parse);
        match r {
            Some(Ok(r)) if r.id == self.lookups[i].txid => self.answered(i, &r, now, random, out),
            _ => alloc::vec![self.finish(i, Answer::Failed, NEGATIVE_TTL, now)],
        }
    }

    fn answered(
        &mut self,
        i: usize,
        r: &dns::Response,
        now: u64,
        random: [u8; 4],
        out: &mut Vec<Send>,
    ) -> Vec<(u32, Answer)> {
        let l = &mut self.lookups[i];
        match dns::found(r, &l.asking, l.qtype) {
            Found::Addresses(a, ttl) => {
                let ttl = l.ttl.min(ttl).min(MAX_TTL);
                alloc::vec![self.finish(i, Answer::Found(a), ttl as u64 * SECOND, now)]
            }
            Found::Alias(target, ttl) if l.aliases < dns::MAX_ALIASES => {
                // The chain left the answers: ask for its end.
                l.aliases += 1;
                l.ttl = l.ttl.min(ttl);
                l.asking = target;
                l.tries = 0;
                l.wait = FIRST_WAIT;
                Resolver::ask(&self.servers, l, now, random, out);
                Vec::new()
            }
            Found::NoSuchName => alloc::vec![self.finish(i, Answer::NoSuchName, NEGATIVE_TTL, now)],
            Found::NoData => alloc::vec![self.finish(i, Answer::NoData, NEGATIVE_TTL, now)],
            _ => alloc::vec![self.finish(i, Answer::Failed, 0, now)],
        }
    }

    fn finish(&mut self, i: usize, answer: Answer, keep: u64, now: u64) -> (u32, Answer) {
        let l = self.lookups.remove(i);
        if keep > 0 {
            if self.cache.len() >= CACHE {
                self.cache.remove(0);
            }
            self.cache.push(Cached { name: l.name, qtype: l.qtype, answer: answer.clone(), until: now + keep });
        }
        (l.id, answer)
    }

    /// Queries due again, and lookups past their deadline.
    pub fn due(&mut self, now: u64, mut random: impl FnMut() -> [u8; 4], out: &mut Vec<Send>) -> Vec<(u32, Answer)> {
        let mut done = Vec::new();
        let mut i = 0;
        while i < self.lookups.len() {
            let l = &mut self.lookups[i];
            if now >= l.deadline {
                done.push(self.finish(i, Answer::NoAnswer, 0, now));
                continue;
            }
            if !l.tcp && now >= l.next {
                if l.tries >= TRIES_PER_SERVER {
                    // This server is silent: the next one, from its first wait.
                    l.server = (l.server + 1) % self.servers.len().max(1);
                    l.tries = 0;
                    l.wait = FIRST_WAIT;
                } else {
                    l.wait *= 2;
                }
                Resolver::ask(&self.servers, l, now, random(), out);
            }
            i += 1;
        }
        done
    }

    pub fn next_wake(&self) -> Option<u64> {
        self.lookups.iter().map(|l| if l.tcp { l.deadline } else { l.next.min(l.deadline) }).min()
    }
}
