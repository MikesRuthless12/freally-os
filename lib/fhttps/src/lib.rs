// SPDX-License-Identifier: Apache-2.0
//! fhttps: an HTTPS GET over netstack (TASK-181): the name resolved by
//! netstack's resolver, a TCP connection, ftls with the roots given (its
//! chain checked, its name the URL's host), fhttp's request and reader; a
//! redirect followed, HTTPS only, at most `REDIRECTS` times. The shell's
//! `fetch` and the e2e door's `tls get` both use it.
//!
//! Bounded: the connection 10 s, each read 5 s of silence, the whole fetch
//! 60 s, the body the caller's limit.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use fstd::ipc::Client;
use fstd::time::{Duration, Instant};
use fwire::proto::net as nt;

/// The redirects one fetch follows.
pub const REDIRECTS: usize = 5;
/// The whole fetch, redirects and all.
const WHOLE: Duration = Duration::from_secs(60);

/// An `https://` URL: its host (lower case), port and path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl Url {
    /// `https://host[:port][/path]`; anything else is refused with why.
    pub fn parse(s: &str) -> Result<Url, String> {
        let rest = s.strip_prefix("https://").ok_or("only https:// URLs are fetched")?;
        let at = rest.find('/').unwrap_or(rest.len());
        let (authority, path) = rest.split_at(at);
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => (h, p.parse::<u16>().ok().filter(|&p| p != 0).ok_or("not a port")?),
            None => (authority, 443),
        };
        let label = |l: &str| {
            !l.is_empty()
                && l.len() <= 63
                && l.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
                && !l.starts_with('-')
                && !l.ends_with('-')
        };
        if host.is_empty() || host.len() > 253 || !host.split('.').all(label) {
            return Err(String::from("not a host name"));
        }
        if path.bytes().any(|c| c <= b' ' || c == 0x7F) {
            return Err(String::from("a space or a control character in the path"));
        }
        Ok(Url { host: host.to_ascii_lowercase(), port, path: String::from(if path.is_empty() { "/" } else { path }) })
    }

    pub fn text(&self) -> String {
        if self.port == 443 {
            alloc::format!("https://{}{}", self.host, self.path)
        } else {
            alloc::format!("https://{}:{}{}", self.host, self.port, self.path)
        }
    }
}

/// What a fetch got: the URL it ended at, each redirect on the way, the
/// cipher suite, and the response.
#[derive(Clone, Debug)]
pub struct Got {
    pub url: String,
    pub hops: Vec<String>,
    pub suite: String,
    pub response: fhttp::Response,
}

/// ftls's refusal in words (`certificate Name`, `alert …`).
pub fn why(e: ftls::Error) -> String {
    match e {
        ftls::Error::Certificate(r) => alloc::format!("certificate {r:?}"),
        ftls::Error::Sent(a) => alloc::format!("sent {a:?}"),
        ftls::Error::Received(a) => alloc::format!("alert {a:?}"),
        other => alloc::format!("{other:?}"),
    }
}

/// `url`, following its redirects: each name through netstack's resolver
/// (its first IPv4 address), checked against `roots`.
pub fn fetch(net: &mut Client, url: &str, roots: &[&[u8]], limit: usize) -> Result<Got, String> {
    let end = Instant::after(WHOLE);
    let mut at = Url::parse(url)?;
    let mut hops = Vec::new();
    loop {
        let to = resolve(net, &at.host, end)?;
        let (suite, response) = get_at(net, to, &at, roots, limit, end)?;
        match fhttp::redirect(&response) {
            Some((host, path)) => {
                if hops.len() == REDIRECTS {
                    return Err(alloc::format!("more than {REDIRECTS} redirects"));
                }
                at = Url { host: host.to_ascii_lowercase(), port: 443, path };
                hops.push(at.text());
            }
            None => return Ok(Got { url: at.text(), hops, suite, response }),
        }
    }
}

fn remote(e: fstd::ipc::CallError) -> String {
    match e {
        fstd::ipc::CallError::Remote(_, why) => why,
        other => alloc::format!("{other:?}"),
    }
}

/// `host`'s first IPv4 address, by netstack's resolver.
fn resolve(net: &mut Client, host: &str, end: u64) -> Result<[u8; 4], String> {
    let q = nt::Resolve { name: String::from(host), kind: 1 };
    let wait = Instant::after(Duration::from_secs(12)).min(end);
    let (r, _) = net.call::<_, nt::Resolved>(nt::RESOLVE, &q, Vec::new(), wait).map_err(remote)?;
    r.addresses
        .iter()
        .find_map(|a| <[u8; 4]>::try_from(a.as_slice()).ok())
        .ok_or_else(|| alloc::format!("{host} has no IPv4 address"))
}

/// One GET of `url` from `to`: the cipher suite and the response.
pub fn get_at(
    net: &mut Client,
    to: [u8; 4],
    url: &Url,
    roots: &[&[u8]],
    limit: usize,
    end: u64,
) -> Result<(String, fhttp::Response), String> {
    let mut entropy = [0u8; 96];
    frandom::fill(&mut entropy).map_err(|_| String::from("no randomness"))?;
    let now = fstd::time::utc_ns() / 1_000_000_000;
    let mut tls = ftls::Client::new(&url.host, roots, now, &entropy).map_err(why)?;
    entropy.fill(0);
    let wait = Instant::after(Duration::from_secs(12)).min(end);
    let connect = nt::Connect { to, port: url.port, timeout_ms: 10_000 };
    let (s, _) = net.call::<_, nt::Socket>(nt::TCP_CONNECT, &connect, Vec::new(), wait).map_err(remote)?;
    let mut c = Conn { net, id: s.id, end };
    let r = c.exchange(&mut tls, url, limit);
    let _ = c.call::<_, ()>(nt::TCP_CLOSE, &nt::Socket { id: s.id }, 2);
    r
}

struct Conn<'a> {
    net: &'a mut Client,
    id: u32,
    end: u64,
}

impl Conn<'_> {
    fn call<Q: fwire::Wire, R: fwire::Wire>(&mut self, ordinal: u32, req: &Q, secs: u64) -> Result<R, String> {
        let wait = Instant::after(Duration::from_secs(secs)).min(self.end);
        self.net.call::<_, R>(ordinal, req, Vec::new(), wait).map(|(r, _)| r).map_err(remote)
    }

    /// Sends what the client has for the server.
    fn flush(&mut self, tls: &mut ftls::Client) -> Result<(), String> {
        let out = tls.take_output();
        for piece in out.chunks(60 * 1024) {
            self.call::<_, nt::Sent>(nt::TCP_SEND, &nt::Send { id: self.id, data: piece.to_vec() }, 10)?;
        }
        Ok(())
    }

    /// The next bytes from the server; empty at its end.
    fn recv(&mut self, what: &str) -> Result<Vec<u8>, String> {
        if Instant::now().as_ns() >= self.end {
            return Err(alloc::format!("the fetch took over {} s", WHOLE.as_secs()));
        }
        let r = self.call::<_, nt::Received>(
            nt::TCP_RECV,
            &nt::Recv { id: self.id, max: 60 * 1024, timeout_ms: 5_000 },
            7,
        )?;
        if r.data.is_empty() && !r.end {
            return Err(alloc::format!("nothing in 5 s {what}"));
        }
        Ok(r.data)
    }

    fn exchange(
        &mut self,
        tls: &mut ftls::Client,
        url: &Url,
        limit: usize,
    ) -> Result<(String, fhttp::Response), String> {
        self.flush(tls)?;
        while !tls.is_connected() {
            let data = self.recv("in the handshake")?;
            if data.is_empty() {
                return Err(String::from("the server closed mid-handshake"));
            }
            let fed = tls.feed(&data);
            self.flush(tls)?;
            fed.map_err(why)?;
        }
        let suite = tls.suite().map_or(String::from("?"), |s| alloc::format!("{s:?}"));
        tls.write(&fhttp::get(&url.host, &url.path)).map_err(why)?;
        self.flush(tls)?;
        let mut http = fhttp::Reader::new(limit);
        while !tls.peer_closed() && http.done().is_none() {
            let data = self.recv("in the response")?;
            if data.is_empty() {
                break; // the TCP connection ended without close_notify
            }
            tls.feed(&data).map_err(why)?;
            self.flush(tls)?;
            http.feed(&tls.take_data()).map_err(|e| alloc::format!("http {e:?}"))?;
        }
        tls.close();
        let _ = self.flush(tls);
        let r = match http.done() {
            Some(r) => r.clone(),
            None => http.close().map_err(|e| alloc::format!("http {e:?}"))?.clone(),
        };
        Ok((suite, r))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_https_with_a_host_an_optional_port_and_a_path() {
        let u = Url::parse("https://Test.Freally.Local/index.html").unwrap();
        assert_eq!((u.host.as_str(), u.port, u.path.as_str()), ("test.freally.local", 443, "/index.html"));
        assert_eq!(Url::parse("https://cdn.example:8443").unwrap().text(), "https://cdn.example:8443/");
        assert_eq!(Url::parse("https://a.b").unwrap().path, "/");
        for bad in [
            "http://test.freally.local/",
            "test.freally.local",
            "https:///x",
            "https://a..b/",
            "https://-a.b/",
            "https://a.b:0/",
            "https://a.b:65536/",
            "https://user@a.b/",
            "https://a.b/two words",
            "https://a_b.c/",
        ] {
            assert!(Url::parse(bad).is_err(), "{bad}");
        }
    }
}
