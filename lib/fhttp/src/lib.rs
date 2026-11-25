// SPDX-License-Identifier: Apache-2.0
//! fhttp (TASK-181): HTTP/1.1 as a client fetches a file (RFC 9110, 9112),
//! sans I/O. [`get`] writes the request (Host, `Connection: close`, no
//! compression asked for); a [`Reader`] takes the response's bytes as they
//! come: the status line and headers (each line at most 8 KiB, at most 100
//! headers), then the body by Content-Length, chunked (RFC 9112 § 7.1), or
//! to the connection's close, never past the limit the caller gives.
//! [`redirect`] reads a redirect, HTTPS only.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

/// The longest status or header line, and chunk-size line.
pub const MAX_LINE: usize = 8 * 1024;
pub const MAX_HEADERS: usize = 100;
/// The most redirects followed for one fetch.
pub const MAX_REDIRECTS: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bad {
    /// Not an HTTP/1.x status line.
    Status,
    /// A header line without its colon, or with a space before it.
    Header,
    /// A line, or the headers, past their limits.
    TooLong,
    /// The body past the caller's limit.
    TooBig,
    /// A chunk's size line isn't hex, or its data isn't followed by CRLF.
    Chunk,
    /// Content-Length isn't one number, or the connection closed short of it.
    Length,
    /// A transfer coding other than chunked.
    Unsupported,
    /// The connection closed before the response did.
    Cut,
}

/// The request: GET `path` from `host`.
pub fn get(host: &str, path: &str) -> Vec<u8> {
    alloc::format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: FreallyOS\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n"
    )
    .into_bytes()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub reason: String,
    /// Names in lower case, values trimmed, in order.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    /// The first header named `name` (lower case).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|h| h.0 == name).map(|h| h.1.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Body {
    Length(usize),
    /// In a chunk with this much left of it, or between chunks (None).
    Chunked(Option<usize>),
    ToClose,
}

#[derive(Debug)]
pub struct Reader {
    limit: usize,
    buf: Vec<u8>,
    head: Option<Response>,
    body: Body,
    done: bool,
    /// The body's bytes so far, taken or not (`take_body`).
    got: usize,
    /// Until the head is whole: how far `buf` has been looked through, where
    /// its last line began, and how many lines ended. Each byte is looked at
    /// once, so a head trickled in costs its length, not its square.
    scanned: usize,
    line_start: usize,
    lines: usize,
}

/// Whether `s` holds a control character other than a tab: RFC 9112's
/// reason-phrase and RFC 9110's field values allow none (the reason is
/// shown as it came: an escape in it would be the Terminal's to obey).
fn has_control(s: &str) -> bool {
    s.chars().any(|c| c != '\t' && c.is_control())
}

/// One CRLF-ended line taken from the front of `buf`, without its CRLF.
fn line(buf: &mut Vec<u8>) -> Result<Option<Vec<u8>>, Bad> {
    match buf.windows(2).position(|w| w == b"\r\n") {
        Some(i) if i > MAX_LINE => Err(Bad::TooLong),
        Some(i) => {
            let l = buf[..i].to_vec();
            buf.drain(..i + 2);
            Ok(Some(l))
        }
        None if buf.len() > MAX_LINE + 1 => Err(Bad::TooLong),
        None => Ok(None),
    }
}

impl Reader {
    /// A reader that takes a body of at most `limit` bytes.
    pub fn new(limit: usize) -> Reader {
        Reader {
            limit,
            buf: Vec::new(),
            head: None,
            body: Body::ToClose,
            done: false,
            got: 0,
            scanned: 0,
            line_start: 0,
            lines: 0,
        }
    }

    /// The body's bytes read since the last take, so a large body passes
    /// through without being held whole (TASK-182: a package on its way to
    /// the disk). The limit still counts every byte of it.
    pub fn take_body(&mut self) -> Vec<u8> {
        self.head.as_mut().map(|r| core::mem::take(&mut r.body)).unwrap_or_default()
    }

    /// The response's head (status and headers), once it's read.
    pub fn head(&self) -> Option<&Response> {
        self.head.as_ref()
    }

    /// The response, once it's whole.
    pub fn done(&self) -> Option<&Response> {
        if self.done { self.head.as_ref() } else { None }
    }

    /// Takes bytes from the connection.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), Bad> {
        if self.done {
            return Ok(()); // anything after the response is ignored
        }
        self.buf.extend_from_slice(bytes);
        if self.head.is_none() && !self.read_head()? {
            return Ok(());
        }
        self.read_body()
    }

    /// The connection closed: the response is whole only if its body was to the close.
    pub fn close(&mut self) -> Result<&Response, Bad> {
        match (self.done, self.body, &self.head) {
            (true, _, Some(r)) => Ok(r),
            (false, Body::ToClose, Some(_)) => {
                self.done = true;
                self.head.as_ref().ok_or(Bad::Cut)
            }
            (_, Body::Length(_), Some(_)) => Err(Bad::Length),
            _ => Err(Bad::Cut),
        }
    }

    fn read_head(&mut self) -> Result<bool, Bad> {
        // Where the head ends (after its empty line), looking only at what's new.
        let mut end = None;
        while self.scanned < self.buf.len() {
            let i = self.scanned;
            self.scanned += 1;
            if self.buf[i] != b'\n' || i == 0 || self.buf[i - 1] != b'\r' {
                continue;
            }
            let len = i - 1 - self.line_start;
            self.line_start = i + 1;
            if len == 0 {
                end = Some(i + 1);
                break;
            }
            self.lines += 1;
            if len > MAX_LINE || self.lines > MAX_HEADERS + 1 {
                return Err(Bad::TooLong);
            }
        }
        let Some(end) = end else {
            return if self.buf.len() - self.line_start > MAX_LINE + 1 { Err(Bad::TooLong) } else { Ok(false) };
        };
        let mut head: Vec<u8> = self.buf.drain(..end).collect();
        let mut lines = Vec::new();
        while let Some(l) = line(&mut head)?
            && !l.is_empty()
        {
            lines.push(l);
        }
        // An empty line first: no status line at all.
        let status = core::str::from_utf8(lines.first().ok_or(Bad::Status)?).map_err(|_| Bad::Status)?;
        let mut parts = status.splitn(3, ' ');
        let (version, code) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        if !matches!(version, "HTTP/1.1" | "HTTP/1.0") || code.len() != 3 || !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Bad::Status);
        }
        let reason = parts.next().unwrap_or("");
        if has_control(reason) {
            return Err(Bad::Status);
        }
        let mut r = Response {
            status: code.parse().map_err(|_| Bad::Status)?,
            reason: String::from(reason),
            headers: Vec::new(),
            body: Vec::new(),
        };
        for l in &lines[1..] {
            let l = core::str::from_utf8(l).map_err(|_| Bad::Header)?;
            let (name, value) = l.split_once(':').ok_or(Bad::Header)?;
            if name.is_empty() || name.ends_with(' ') || name.ends_with('\t') {
                return Err(Bad::Header); // RFC 9112 § 5.1: no space before the colon
            }
            if has_control(l) {
                return Err(Bad::Header);
            }
            r.headers.push((name.to_ascii_lowercase(), String::from(value.trim())));
        }
        let te = r.header("transfer-encoding").map(str::to_ascii_lowercase);
        self.body = match (te.as_deref(), r.header("content-length")) {
            (Some("chunked"), _) => Body::Chunked(None),
            (Some(_), _) => return Err(Bad::Unsupported),
            (None, Some(n)) => {
                // Repeated lengths must agree (§ 6.3); a list of equal values counts as one.
                let mut vals = r.headers.iter().filter(|h| h.0 == "content-length").flat_map(|h| h.1.split(','));
                let first = vals.next().map(str::trim).ok_or(Bad::Length)?;
                if vals.any(|v| v.trim() != first) || n.is_empty() || !first.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(Bad::Length);
                }
                let len: usize = first.parse().map_err(|_| Bad::Length)?;
                if len > self.limit {
                    return Err(Bad::TooBig);
                }
                Body::Length(len)
            }
            // No body by definition: 1xx, 204, 304.
            (None, None) if r.status / 100 == 1 || r.status == 204 || r.status == 304 => Body::Length(0),
            (None, None) => Body::ToClose,
        };
        self.head = Some(r);
        Ok(true)
    }

    fn read_body(&mut self) -> Result<(), Bad> {
        let limit = self.limit;
        let Some(r) = self.head.as_mut() else { return Ok(()) };
        loop {
            match self.body {
                Body::Length(n) => {
                    let take = (n - self.got).min(self.buf.len());
                    r.body.extend(self.buf.drain(..take));
                    self.got += take;
                    if self.got == n {
                        self.done = true;
                    }
                    return Ok(());
                }
                Body::ToClose => {
                    if self.got + self.buf.len() > limit {
                        return Err(Bad::TooBig);
                    }
                    self.got += self.buf.len();
                    r.body.append(&mut self.buf);
                    return Ok(());
                }
                Body::Chunked(None) => {
                    let Some(l) = line(&mut self.buf)? else { return Ok(()) };
                    let l = core::str::from_utf8(&l).map_err(|_| Bad::Chunk)?;
                    let size = l.split(';').next().unwrap_or("").trim();
                    if size.is_empty() || size.len() > 15 {
                        return Err(Bad::Chunk);
                    }
                    let n = usize::from_str_radix(size, 16).map_err(|_| Bad::Chunk)?;
                    if n == 0 {
                        // The last chunk: trailers (skipped) up to the empty line.
                        loop {
                            match line(&mut self.buf)? {
                                None => {
                                    self.body = Body::Chunked(Some(0));
                                    return Ok(());
                                }
                                Some(t) if t.is_empty() => {
                                    self.done = true;
                                    return Ok(());
                                }
                                Some(_) => {}
                            }
                        }
                    }
                    if self.got + n > limit {
                        return Err(Bad::TooBig);
                    }
                    self.body = Body::Chunked(Some(n + 2)); // its data, then CRLF
                }
                Body::Chunked(Some(0)) => {
                    // After the last chunk, still in its trailers.
                    loop {
                        match line(&mut self.buf)? {
                            None => return Ok(()),
                            Some(t) if t.is_empty() => {
                                self.done = true;
                                return Ok(());
                            }
                            Some(_) => {}
                        }
                    }
                }
                Body::Chunked(Some(left)) => {
                    if self.buf.len() < left {
                        return Ok(());
                    }
                    let data: Vec<u8> = self.buf.drain(..left).collect();
                    if !data.ends_with(b"\r\n") {
                        return Err(Bad::Chunk);
                    }
                    r.body.extend_from_slice(&data[..left - 2]);
                    self.got += left - 2;
                    self.body = Body::Chunked(None);
                }
            }
        }
    }
}

/// Where a redirect (301, 302, 303, 307, 308) sends a fetch: (host, path),
/// HTTPS only; None for anything else. The host is a name (letters,
/// digits, `-` and `.`: no credentials, port or query), the path printable
/// ASCII without a space (it goes into the next request's line as it is),
/// its fragment dropped.
pub fn redirect(r: &Response) -> Option<(String, String)> {
    if !matches!(r.status, 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    let rest = r.header("location")?.strip_prefix("https://")?;
    let rest = rest.split('#').next().unwrap_or("");
    let host = rest.split(['/', '?']).next().unwrap_or("");
    let path = &rest[host.len()..];
    let named = |b: u8| b.is_ascii_alphanumeric() || b == b'-' || b == b'.';
    if host.is_empty() || !host.bytes().all(named) || !path.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    let path = match path {
        "" => String::from("/"),
        p if p.starts_with('?') => alloc::format!("/{p}"),
        p => String::from(p),
    };
    Some((String::from(host), path))
}

#[cfg(test)]
mod tests;
