// SPDX-License-Identifier: Apache-2.0
//! HTTP/1.1 (just enough for `/json/*` and the WebSocket upgrade), WebSocket
//! framing (RFC 6455) for both ends, and a CDP connection on top. Host only.

use fjson::Value;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;

pub mod qmp;

/// The GUID RFC 6455 § 1.3 appends to the client's key.
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/// Messages larger than this are refused (a screenshot is a few MB).
pub const MAX_MESSAGE: usize = 64 << 20;

/// An HTTP request line and its headers.
#[derive(Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// Reads a request's head (no body: `/json/*` and upgrades have none).
pub fn read_request(r: &mut impl BufRead) -> io::Result<Request> {
    let mut line = String::new();
    r.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        if headers.len() > 64 || h.len() > 8192 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "headers too large"));
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    Ok(Request { method, path, headers })
}

pub fn respond(w: &mut impl Write, status: &str, content_type: &str, body: &[u8]) -> io::Result<()> {
    write!(
        w,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    w.write_all(body)?;
    w.flush()
}

/// `Sec-WebSocket-Accept` for a client key (RFC 6455 § 4.2.2).
pub fn accept_key(key: &str) -> String {
    let mut s = String::from(key.trim());
    s.push_str(WS_GUID);
    fjson::base64::encode(&fcrypto::sha1::sha1_for_websocket_handshake_only(s.as_bytes()))
}

/// One WebSocket connection. A client masks what it sends; a server doesn't.
pub struct WebSocket {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    is_client: bool,
    mask_state: u64,
}

pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    Close,
}

impl WebSocket {
    /// Completes the server side of the handshake for `req`, read from `reader`.
    pub fn accept(reader: BufReader<TcpStream>, req: &Request) -> io::Result<WebSocket> {
        let key =
            req.header("Sec-WebSocket-Key").ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no key"))?;
        let mut writer = reader.get_ref().try_clone()?;
        write!(
            writer,
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
            accept_key(key)
        )?;
        writer.flush()?;
        Ok(WebSocket { reader, writer, is_client: false, mask_state: 0 })
    }

    /// Connects to `ws://host:port/path` as a client.
    pub fn connect(host_port: &str, path: &str) -> io::Result<WebSocket> {
        let stream = TcpStream::connect(host_port)?;
        stream.set_nodelay(true)?;
        let mut writer = stream.try_clone()?;
        let mut seed = [0u8; 16];
        // The key and the masks only need to be unpredictable to a proxy; this is
        // the host's own loopback, but read the OS's randomness anyway.
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            let _ = f.read_exact(&mut seed);
        }
        let key = fjson::base64::encode(&seed);
        write!(
            writer,
            "GET {path} HTTP/1.1\r\nHost: {host_port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )?;
        writer.flush()?;
        let mut reader = BufReader::new(stream);
        let mut status = String::new();
        reader.read_line(&mut status)?;
        if !status.contains(" 101 ") {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("not upgraded: {}", status.trim())));
        }
        let mut accepted = None;
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h)? == 0 || h.trim().is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':')
                && k.trim().eq_ignore_ascii_case("Sec-WebSocket-Accept")
            {
                accepted = Some(v.trim().to_string());
            }
        }
        if accepted.as_deref() != Some(accept_key(&key).as_str()) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "wrong Sec-WebSocket-Accept"));
        }
        let mask_state = u64::from_le_bytes(seed[..8].try_into().unwrap_or([7; 8])) | 1;
        Ok(WebSocket { reader, writer, is_client: true, mask_state })
    }

    fn next_mask(&mut self) -> [u8; 4] {
        let mut x = self.mask_state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.mask_state = x;
        (x as u32).to_le_bytes()
    }

    fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> io::Result<()> {
        let mut head = Vec::with_capacity(14);
        head.push(0x80 | opcode);
        let mask_bit = if self.is_client { 0x80 } else { 0 };
        match payload.len() {
            n if n < 126 => head.push(mask_bit | n as u8),
            n if n <= 0xFFFF => {
                head.push(mask_bit | 126);
                head.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                head.push(mask_bit | 127);
                head.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        if self.is_client {
            let m = self.next_mask();
            head.extend_from_slice(&m);
            let masked: Vec<u8> = payload.iter().enumerate().map(|(i, b)| b ^ m[i % 4]).collect();
            self.writer.write_all(&head)?;
            self.writer.write_all(&masked)?;
        } else {
            self.writer.write_all(&head)?;
            self.writer.write_all(payload)?;
        }
        self.writer.flush()
    }

    pub fn send_text(&mut self, text: &str) -> io::Result<()> {
        self.send_frame(0x1, text.as_bytes())
    }

    /// Sends a close frame (RFC 6455 § 5.5.1).
    pub fn close(&mut self) -> io::Result<()> {
        self.send_frame(0x8, &[])
    }

    /// The next whole message (fragments joined; pings answered).
    pub fn recv(&mut self) -> io::Result<Message> {
        let mut data = Vec::new();
        let mut first_op = None;
        loop {
            let mut h = [0u8; 2];
            self.reader.read_exact(&mut h)?;
            let fin = h[0] & 0x80 != 0;
            let op = h[0] & 0x0F;
            let masked = h[1] & 0x80 != 0;
            let mut len = (h[1] & 0x7F) as u64;
            if len == 126 {
                let mut b = [0u8; 2];
                self.reader.read_exact(&mut b)?;
                len = u16::from_be_bytes(b) as u64;
            } else if len == 127 {
                let mut b = [0u8; 8];
                self.reader.read_exact(&mut b)?;
                len = u64::from_be_bytes(b);
            }
            if data.len() as u64 + len > MAX_MESSAGE as u64 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "message too large"));
            }
            // RFC 6455 § 5.1: clients must mask, servers must not.
            if masked == self.is_client {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "wrong masking"));
            }
            let mut mask = [0u8; 4];
            if masked {
                self.reader.read_exact(&mut mask)?;
            }
            let mut payload = vec![0u8; len as usize];
            self.reader.read_exact(&mut payload)?;
            if masked {
                for (i, b) in payload.iter_mut().enumerate() {
                    *b ^= mask[i % 4];
                }
            }
            match op {
                0x8 => {
                    let _ = self.send_frame(0x8, &[]);
                    return Ok(Message::Close);
                }
                0x9 => {
                    self.send_frame(0xA, &payload)?;
                    continue;
                }
                0xA => continue,
                0x0..=0x2 => {
                    if op != 0 {
                        first_op = Some(op);
                    }
                    data.extend_from_slice(&payload);
                    if fin {
                        return Ok(match first_op {
                            Some(0x2) => Message::Binary(data),
                            _ => Message::Text(
                                String::from_utf8(data)
                                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "text isn't UTF-8"))?,
                            ),
                        });
                    }
                }
                _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "unknown opcode")),
            }
        }
    }
}

/// A CDP client: `call` sends `{id, method, params}` and waits for its reply.
pub struct Cdp {
    ws: WebSocket,
    next_id: i64,
}

impl Drop for Cdp {
    fn drop(&mut self) {
        let _ = self.ws.close();
    }
}

impl Cdp {
    /// Connects to the first target `/json/list` gives at `host_port`.
    pub fn connect(host_port: &str) -> io::Result<Cdp> {
        let list = http_get(host_port, "/json/list")?;
        let v = fjson::parse(&list)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("/json/list: {e:?}")))?;
        let url = v
            .as_array()
            .and_then(|a| a.first())
            .and_then(|t| t.get("webSocketDebuggerUrl"))
            .and_then(Value::as_str)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no target"))?;
        let rest = url.strip_prefix("ws://").ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not ws://"))?;
        let (hp, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        Ok(Cdp { ws: WebSocket::connect(hp, path)?, next_id: 1 })
    }

    /// Calls `method`; `Err` carries CDP's error message.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = Value::obj([("id", id.into()), ("method", method.into()), ("params", params)]);
        self.ws.send_text(&msg.to_json()).map_err(|e| e.to_string())?;
        loop {
            match self.ws.recv().map_err(|e| e.to_string())? {
                Message::Text(t) => {
                    let v = fjson::parse(&t).map_err(|e| format!("bad reply: {e:?}"))?;
                    if v.get("id").and_then(Value::as_i64) != Some(id) {
                        continue; // an event, or another call's reply
                    }
                    if let Some(e) = v.get("error") {
                        return Err(e.get("message").and_then(Value::as_str).unwrap_or("error").to_string());
                    }
                    return Ok(v.get("result").cloned().unwrap_or(Value::Obj(Vec::new())));
                }
                Message::Binary(_) => continue,
                Message::Close => return Err("the connection closed".into()),
            }
        }
    }
}

/// A plain HTTP GET of `path`; the body as text.
pub fn http_get(host_port: &str, path: &str) -> io::Result<String> {
    let mut s = TcpStream::connect(host_port)?;
    write!(s, "GET {path} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\r\n")?;
    let mut text = String::new();
    s.read_to_string(&mut text)?;
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    if !head.starts_with("HTTP/1.1 200") {
        return Err(io::Error::other(head.lines().next().unwrap_or("no status").to_string()));
    }
    Ok(body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_6455_accept_key_example() {
        // RFC 6455 § 1.3's worked example.
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }
}
