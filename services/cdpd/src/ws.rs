// SPDX-License-Identifier: Apache-2.0
//! WebSocket (RFC 6455) for cdpd's port on the network (TASK-184): the
//! opening handshake as a server gives it, and frames read and written. A
//! server's side only: a client's frames must be masked (§ 5.1) and a
//! server's never are; a message is at most [`MAX_MESSAGE`]; fragments are
//! put back together, with control frames (close, ping, pong) allowed
//! between them (§ 5.4). No extensions, no subprotocols.

use alloc::string::String;
use alloc::vec::Vec;

/// § 1.3's GUID, appended to the client's key for the accept key.
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/// The longest message taken from a client (CDP's commands are small).
pub const MAX_MESSAGE: usize = 1 << 20;
/// The longest opening handshake taken.
pub const MAX_HANDSHAKE: usize = 8 * 1024;

/// Opcodes (§ 5.2).
pub mod op {
    pub const CONTINUATION: u8 = 0;
    pub const TEXT: u8 = 1;
    pub const BINARY: u8 = 2;
    pub const CLOSE: u8 = 8;
    pub const PING: u8 = 9;
    pub const PONG: u8 = 10;
}

/// Why a connection is refused or failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not an opening handshake this server takes.
    Handshake(&'static str),
    /// A frame or message RFC 6455 refuses.
    Protocol(&'static str),
    /// A message longer than [`MAX_MESSAGE`].
    TooBig,
}

/// `Sec-WebSocket-Accept` for a client's `Sec-WebSocket-Key` (§ 4.2.2).
pub fn accept_key(key: &str) -> String {
    let mut k = String::from(key);
    k.push_str(GUID);
    fjson::base64::encode(&fcrypto::sha1::sha1_for_websocket_handshake_only(k.as_bytes()))
}

/// The opening handshake (§ 4.2.1) once `buf` holds it whole: the path
/// asked for, the response to send, and how many bytes of `buf` it took.
/// `Ok(None)` while it's still arriving.
pub fn handshake(buf: &[u8]) -> Result<Option<(String, Vec<u8>, usize)>, Error> {
    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        return if buf.len() > MAX_HANDSHAKE { Err(Error::Handshake("too long")) } else { Ok(None) };
    };
    if end > MAX_HANDSHAKE {
        return Err(Error::Handshake("too long"));
    }
    let head = core::str::from_utf8(&buf[..end]).map_err(|_| Error::Handshake("not text"))?;
    let mut lines = head.split("\r\n");
    let mut request = lines.next().unwrap_or("").split(' ');
    let (Some("GET"), Some(path), Some("HTTP/1.1"), None) =
        (request.next(), request.next(), request.next(), request.next())
    else {
        return Err(Error::Handshake("not GET <path> HTTP/1.1"));
    };
    let (mut key, mut version, mut upgrade, mut connection) = (None, None, false, false);
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(Error::Handshake("a header without a colon"))?;
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "sec-websocket-key" => key = Some(value),
            "sec-websocket-version" => version = Some(value),
            "upgrade" => upgrade = value.eq_ignore_ascii_case("websocket"),
            "connection" => connection = value.split(',').any(|t| t.trim().eq_ignore_ascii_case("upgrade")),
            _ => {}
        }
    }
    if !upgrade || !connection {
        return Err(Error::Handshake("not an upgrade to websocket"));
    }
    if version != Some("13") {
        return Err(Error::Handshake("version 13 only"));
    }
    let key = key.ok_or(Error::Handshake("no Sec-WebSocket-Key"))?;
    // § 4.1: 16 random bytes, base64.
    if fjson::base64::decode(key).map(|k| k.len()) != Some(16) {
        return Err(Error::Handshake("a key that isn't 16 bytes"));
    }
    let response = alloc::format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        accept_key(key)
    );
    Ok(Some((String::from(path), response.into_bytes(), end + 4)))
}

/// A frame from a client (§ 5.2), its payload unmasked.
#[derive(Debug, PartialEq, Eq)]
pub struct Frame {
    pub fin: bool,
    pub op: u8,
    pub payload: Vec<u8>,
}

/// A client's next frame from `buf`, and how many bytes it took; `Ok(None)`
/// while it's still arriving.
pub fn read_frame(buf: &[u8]) -> Result<Option<(Frame, usize)>, Error> {
    let [b0, b1, ..] = *buf else { return Ok(None) };
    if b0 & 0x70 != 0 {
        return Err(Error::Protocol("a reserved bit set"));
    }
    let (fin, op) = (b0 & 0x80 != 0, b0 & 0x0F);
    if !matches!(op, op::CONTINUATION | op::TEXT | op::BINARY | op::CLOSE | op::PING | op::PONG) {
        return Err(Error::Protocol("a reserved opcode"));
    }
    if b1 & 0x80 == 0 {
        return Err(Error::Protocol("a client's frame unmasked"));
    }
    let (len, at) = match b1 & 0x7F {
        126 => match buf.get(2..4) {
            Some(b) => (u64::from(u16::from_be_bytes([b[0], b[1]])), 4),
            None => return Ok(None),
        },
        127 => match buf.get(2..10) {
            Some(b) => (u64::from_be_bytes(b.try_into().expect("8 bytes")), 10),
            None => return Ok(None),
        },
        n => (u64::from(n), 2),
    };
    if op >= op::CLOSE && (!fin || len > 125) {
        return Err(Error::Protocol("a control frame fragmented or over 125 bytes"));
    }
    if len > MAX_MESSAGE as u64 {
        return Err(Error::TooBig);
    }
    let total = at + 4 + len as usize;
    if buf.len() < total {
        return Ok(None);
    }
    let mask = [buf[at], buf[at + 1], buf[at + 2], buf[at + 3]];
    let payload = buf[at + 4..total].iter().enumerate().map(|(i, b)| b ^ mask[i % 4]).collect();
    Ok(Some((Frame { fin, op, payload }, total)))
}

/// A server's frame (§ 5.2): whole (FIN), unmasked, the shortest length that fits.
pub fn frame(op: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 10);
    out.push(0x80 | op);
    match payload.len() {
        n @ 0..=125 => out.push(n as u8),
        n @ 126..=0xFFFF => {
            out.push(126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(payload);
    out
}

/// What a client sent, a message or a control frame.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Text(String),
    Binary(Vec<u8>),
    /// Answered with a pong carrying the same bytes (§ 5.5.2).
    Ping(Vec<u8>),
    Pong,
    /// Its status code, if it gave one; answered with a close (§ 5.5.1).
    Close(Option<u16>),
}

/// A client's bytes, made into messages (§ 5.4).
#[derive(Default)]
pub struct Reader {
    buf: Vec<u8>,
    /// A fragmented message so far: its opcode and its bytes.
    partial: Option<(u8, Vec<u8>)>,
}

impl Reader {
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next message or control frame, `Ok(None)` until one is whole.
    /// After an error the connection is failed (§ 7.1.7): nothing more is read.
    pub fn event(&mut self) -> Result<Option<Event>, Error> {
        loop {
            let Some((f, n)) = read_frame(&self.buf)? else { return Ok(None) };
            self.buf.drain(..n);
            let (op, data) = match f.op {
                op::PING => return Ok(Some(Event::Ping(f.payload))),
                op::PONG => return Ok(Some(Event::Pong)),
                op::CLOSE => {
                    return match f.payload.as_slice() {
                        [] => Ok(Some(Event::Close(None))),
                        [a, b, ..] => Ok(Some(Event::Close(Some(u16::from_be_bytes([*a, *b]))))),
                        [_] => Err(Error::Protocol("a close with a one-byte body")),
                    };
                }
                op::CONTINUATION => {
                    let Some((_, data)) = &mut self.partial else {
                        return Err(Error::Protocol("a continuation with nothing to continue"));
                    };
                    if data.len() + f.payload.len() > MAX_MESSAGE {
                        return Err(Error::TooBig);
                    }
                    data.extend_from_slice(&f.payload);
                    if !f.fin {
                        continue;
                    }
                    self.partial.take().expect("a message in pieces")
                }
                op => {
                    if self.partial.is_some() {
                        return Err(Error::Protocol("a new message inside a fragmented one"));
                    }
                    if !f.fin {
                        self.partial = Some((op, f.payload));
                        continue;
                    }
                    (op, f.payload)
                }
            };
            return match op {
                op::TEXT => String::from_utf8(data)
                    .map(|t| Some(Event::Text(t)))
                    .map_err(|_| Error::Protocol("text that isn't UTF-8")),
                _ => Ok(Some(Event::Binary(data))),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// A client's frame: `op`, FIN as given, masked with `key`.
    fn masked(fin: bool, op: u8, payload: &[u8], key: [u8; 4]) -> Vec<u8> {
        let mut f = frame(op, payload);
        if !fin {
            f[0] &= 0x7F;
        }
        f[1] |= 0x80;
        let at = match f[1] & 0x7F {
            126 => 4,
            127 => 10,
            _ => 2,
        };
        let body: Vec<u8> = f.split_off(at).iter().enumerate().map(|(i, b)| b ^ key[i % 4]).collect();
        f.extend_from_slice(&key);
        f.extend(body);
        f
    }

    /// § 1.3's example key and its answer.
    #[test]
    fn the_accept_key_is_the_rfcs() {
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    /// § 1.2's request (version 13), whole, in pieces, and with each thing
    /// it needs taken away.
    #[test]
    fn the_opening_handshake() {
        let req = "GET /chat HTTP/1.1\r\nHost: server.example.com\r\nUpgrade: websocket\r\nConnection: keep-alive, Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nOrigin: http://example.com\r\nSec-WebSocket-Version: 13\r\n\r\n";
        let mut with_frame = req.as_bytes().to_vec();
        with_frame.extend_from_slice(b"\x81");
        let (path, response, used) = handshake(&with_frame).unwrap().unwrap();
        assert_eq!((path.as_str(), used), ("/chat", req.len()));
        assert_eq!(
            core::str::from_utf8(&response).unwrap(),
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n"
        );
        assert_eq!(handshake(&req.as_bytes()[..req.len() - 1]), Ok(None), "still arriving");
        for (from, to, why) in [
            ("GET /chat HTTP/1.1", "POST /chat HTTP/1.1", "not GET <path> HTTP/1.1"),
            ("Upgrade: websocket", "Upgrade: h2c", "not an upgrade to websocket"),
            ("Connection: keep-alive, Upgrade", "Connection: keep-alive", "not an upgrade to websocket"),
            ("Sec-WebSocket-Version: 13", "Sec-WebSocket-Version: 8", "version 13 only"),
            ("Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n", "", "no Sec-WebSocket-Key"),
            ("dGhlIHNhbXBsZSBub25jZQ==", "c2hvcnQ=", "a key that isn't 16 bytes"),
            ("Host: server.example.com", "Host server.example.com", "a header without a colon"),
        ] {
            assert_eq!(handshake(req.replace(from, to).as_bytes()), Err(Error::Handshake(why)), "{to:?}");
        }
        assert_eq!(handshake(&vec![b'a'; MAX_HANDSHAKE + 1]), Err(Error::Handshake("too long")));
    }

    /// § 5.7's examples: a server's frames byte for byte, a client's read.
    #[test]
    fn the_rfcs_frames() {
        assert_eq!(frame(op::TEXT, b"Hello"), [0x81, 0x05, 0x48, 0x65, 0x6c, 0x6c, 0x6f]);
        assert_eq!(frame(op::BINARY, &[0; 256])[..4], [0x82, 0x7E, 0x01, 0x00]);
        assert_eq!(frame(op::BINARY, &[0; 65536])[..10], [0x82, 0x7F, 0, 0, 0, 0, 0, 1, 0, 0]);
        let hello = [0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58];
        let want = Frame { fin: true, op: op::TEXT, payload: b"Hello".to_vec() };
        assert_eq!(read_frame(&hello), Ok(Some((want, 11))));
        assert_eq!(masked(true, op::TEXT, b"Hello", [0x37, 0xfa, 0x21, 0x3d]), hello);
        for n in 0..hello.len() {
            assert_eq!(read_frame(&hello[..n]), Ok(None), "{n} bytes");
        }
        // Long lengths, read back.
        for n in [125, 126, 65535, 65536] {
            let f = masked(true, op::BINARY, &vec![7; n], [1, 2, 3, 4]);
            let (got, used) = read_frame(&f).unwrap().unwrap();
            assert_eq!((got.payload.len(), used), (n, f.len()));
        }
    }

    #[test]
    fn frames_rfc_6455_refuses() {
        let key = [9, 8, 7, 6];
        assert_eq!(read_frame(&frame(op::TEXT, b"Hello")), Err(Error::Protocol("a client's frame unmasked")));
        let mut rsv = masked(true, op::TEXT, b"x", key);
        rsv[0] |= 0x40;
        assert_eq!(read_frame(&rsv), Err(Error::Protocol("a reserved bit set")));
        let mut reserved = masked(true, op::TEXT, b"x", key);
        reserved[0] = 0x83;
        assert_eq!(read_frame(&reserved), Err(Error::Protocol("a reserved opcode")));
        let control = Err(Error::Protocol("a control frame fragmented or over 125 bytes"));
        assert_eq!(read_frame(&masked(false, op::PING, b"x", key)), control);
        assert_eq!(read_frame(&masked(true, op::PING, &[0; 126], key)), control);
        let mut huge = masked(true, op::BINARY, &[], key);
        huge.splice(1..2, [0xFF, 0, 0, 0, 0, 0, 0x20, 0, 0]);
        assert_eq!(read_frame(&huge), Err(Error::TooBig));
    }

    /// § 5.4: a message in pieces, a ping between them answered at once, the
    /// close with its code; the errors a reader fails on.
    #[test]
    fn messages_put_back_together() {
        let key = [1, 2, 3, 4];
        let mut r = Reader::default();
        let mut bytes = masked(false, op::TEXT, b"Hel", key);
        bytes.extend(masked(true, op::PING, b"p", key));
        bytes.extend(masked(true, op::CONTINUATION, b"lo", key));
        bytes.extend(masked(true, op::BINARY, &[1, 2], key));
        bytes.extend(masked(true, op::CLOSE, &1000u16.to_be_bytes(), key));
        // Fed a byte at a time: nothing until each is whole.
        let mut got = Vec::new();
        for b in bytes {
            r.feed(&[b]);
            while let Some(e) = r.event().unwrap() {
                got.push(e);
            }
        }
        assert_eq!(
            got,
            [
                Event::Ping(b"p".to_vec()),
                Event::Text(String::from("Hello")),
                Event::Binary(vec![1, 2]),
                Event::Close(Some(1000))
            ]
        );
        for (frames, why) in [
            (masked(true, op::CONTINUATION, b"x", key), "a continuation with nothing to continue"),
            (
                [masked(false, op::TEXT, b"a", key), masked(true, op::TEXT, b"b", key)].concat(),
                "a new message inside a fragmented one",
            ),
            (masked(true, op::TEXT, &[0xC3, 0x28], key), "text that isn't UTF-8"),
            (masked(true, op::CLOSE, &[3], key), "a close with a one-byte body"),
        ] {
            let mut r = Reader::default();
            r.feed(&frames);
            assert_eq!(r.event(), Err(Error::Protocol(why)));
        }
        // Pieces adding up past the most a message holds.
        let mut r = Reader::default();
        r.feed(&masked(false, op::BINARY, &vec![0; MAX_MESSAGE - 1], key));
        r.feed(&masked(true, op::CONTINUATION, &[0; 2], key));
        assert_eq!(r.event(), Err(Error::TooBig));
    }

    /// The fuzz target: frames and messages changed, cut or added to, fed in
    /// random pieces; nothing panics. `ci --slow` runs 2,000 (`FREALLY_FUZZ_ITERS`).
    #[test]
    fn changed_frames_never_panic() {
        extern crate std;
        let iters: u64 = std::env::var("FREALLY_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let key = [5, 6, 7, 8];
        let seed = [
            masked(false, op::TEXT, b"{\"id\":1,", key),
            masked(true, op::PING, b"ping", key),
            masked(true, op::CONTINUATION, b"\"method\":\"DOM.getDocument\"}", key),
            masked(true, op::BINARY, &[0; 300], key),
            masked(true, op::CLOSE, &1000u16.to_be_bytes(), key),
        ]
        .concat();
        for _ in 0..iters {
            let mut b = seed.clone();
            for _ in 0..1 + next() % 4 {
                let at = next() as usize % b.len();
                match next() % 3 {
                    0 => b[at] ^= 1 + (next() % 255) as u8,
                    1 => b.truncate(at.max(1)),
                    _ => b.insert(at, next() as u8),
                }
            }
            let mut r = Reader::default();
            let mut at = 0;
            'feed: while at < b.len() {
                let end = (at + 1 + next() as usize % 64).min(b.len());
                r.feed(&b[at..end]);
                at = end;
                loop {
                    match r.event() {
                        Ok(Some(_)) => {}
                        Ok(None) => break,
                        Err(_) => break 'feed,
                    }
                }
            }
            let _ = handshake(&b);
        }
    }
}
