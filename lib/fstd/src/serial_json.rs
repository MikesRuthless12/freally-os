// SPDX-License-Identifier: Apache-2.0
//! Newline-delimited framing for serial lines (TASK-044): the Brain (COM2)
//! and cdpd (COM3) speak one JSON text per line; the e2e test door frames
//! its command lines the same way.
//!
//! - Sending (`frame`): the text must be one line. JSON escapes newlines
//!   inside strings, so a compact encoder never writes one; a text that has
//!   one is refused rather than sent as two messages.
//! - Receiving (`Framer`): bytes arrive in any pieces; each complete line is
//!   one message, `\r\n` or `\n`. A partial line the far end abandons (the
//!   line drops mid-message) is discarded once nothing has arrived for
//!   `STALE_NS`, so it can't glue itself onto the next message (the PRD's
//!   "Serial line drops mid-message": the partial line is discarded, and
//!   the client's request times out). A line longer than `MAX_LINE` is
//!   discarded whole.

use alloc::string::String;
use alloc::vec::Vec;

/// The longest line accepted.
pub const MAX_LINE: usize = 1 << 20;
/// A partial line older than this is abandoned.
pub const STALE_NS: u64 = 1_000_000_000;

/// Why a line was thrown away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dropped {
    /// Longer than `MAX_LINE`.
    TooLong,
    /// Not UTF-8 (so not JSON).
    NotUtf8,
    /// Abandoned mid-line: nothing more arrived for `STALE_NS`.
    Stale,
}

/// A message as its line: the text and the newline that ends it.
pub fn frame(text: &str) -> Result<Vec<u8>, &'static str> {
    if text.bytes().any(|b| b == b'\n' || b == b'\r') {
        return Err("a message must be one line (escape newlines inside strings)");
    }
    if text.len() > MAX_LINE {
        return Err("a message is longer than a line may be");
    }
    let mut v = Vec::with_capacity(text.len() + 1);
    v.extend_from_slice(text.as_bytes());
    v.push(b'\n');
    Ok(v)
}

/// Reassembles lines from bytes as they arrive.
#[derive(Default)]
pub struct Framer {
    buf: Vec<u8>,
    /// The current line passed `MAX_LINE`: drop the rest of it.
    overflow: bool,
    last_ns: u64,
}

impl Framer {
    pub fn new() -> Framer {
        Framer::default()
    }

    /// Takes bytes that arrived at `now_ns` up to the end of the first line
    /// they complete. Returns that line (or why it was dropped), if one
    /// ended, and how many bytes were taken: the rest belongs to whatever
    /// the line said comes next.
    pub fn next(&mut self, bytes: &[u8], now_ns: u64) -> (Option<Result<String, Dropped>>, usize) {
        if !self.buf.is_empty() && now_ns.saturating_sub(self.last_ns) > STALE_NS {
            self.buf.clear();
            self.overflow = false;
            self.last_ns = now_ns;
            return (Some(Err(Dropped::Stale)), 0);
        }
        self.last_ns = now_ns;
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' {
                let mut line = core::mem::take(&mut self.buf);
                if core::mem::take(&mut self.overflow) {
                    return (Some(Err(Dropped::TooLong)), i + 1);
                }
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if line.is_empty() {
                    continue;
                }
                let r = String::from_utf8(line).map_err(|_| Dropped::NotUtf8);
                return (Some(r), i + 1);
            }
            if self.overflow {
                continue;
            }
            if self.buf.len() >= MAX_LINE {
                self.overflow = true;
                self.buf.clear();
            } else {
                self.buf.push(b);
            }
        }
        (None, bytes.len())
    }

    /// Every line `bytes` completes, in order.
    pub fn push(&mut self, mut bytes: &[u8], now_ns: u64) -> Vec<Result<String, Dropped>> {
        let mut out = Vec::new();
        loop {
            let (line, used) = self.next(bytes, now_ns);
            bytes = &bytes[used..];
            match line {
                Some(l) => out.push(l),
                None => return out,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;

    #[test]
    fn lines_come_out_whole_however_the_bytes_arrive() {
        let mut f = Framer::new();
        assert_eq!(f.push(b"{\"a\":", 0), vec![]);
        assert_eq!(f.push(b"1}\n{\"b\":2}\r\n{\"c\"", 1), vec![Ok("{\"a\":1}".into()), Ok("{\"b\":2}".into())]);
        assert_eq!(f.push(b":3}\n", 2), vec![Ok("{\"c\":3}".into())]);
        // Blank lines are nothing.
        assert_eq!(f.push(b"\n\r\n", 3), vec![]);
    }

    #[test]
    fn next_stops_after_the_first_line() {
        let mut f = Framer::new();
        let (line, used) = f.next(b"echo 3\nabc", 0);
        assert_eq!((line, used), (Some(Ok("echo 3".into())), 7));
    }

    #[test]
    fn an_abandoned_partial_line_is_discarded() {
        let mut f = Framer::new();
        assert_eq!(f.push(b"{\"half\":", 0), vec![]);
        // The line dropped; the next message starts after a pause.
        assert_eq!(f.push(b"{\"next\":1}\n", STALE_NS + 1), vec![Err(Dropped::Stale), Ok("{\"next\":1}".into())]);
        // Within the pause it's still the same line.
        assert_eq!(f.push(b"{\"x\":", 10 * STALE_NS), vec![]);
        assert_eq!(f.push(b"1}\n", 10 * STALE_NS + 5), vec![Ok("{\"x\":1}".into())]);
    }

    #[test]
    fn too_long_and_not_utf8_are_dropped_and_the_next_line_survives() {
        let mut f = Framer::new();
        let long = vec![b'x'; MAX_LINE + 10];
        assert_eq!(f.push(&long, 0), vec![]);
        assert_eq!(f.push(b"tail\n{\"ok\":1}\n", 1), vec![Err(Dropped::TooLong), Ok("{\"ok\":1}".into())]);
        assert_eq!(f.push(b"\xff\xfe\n", 2), vec![Err(Dropped::NotUtf8)]);
    }

    #[test]
    fn frame_refuses_what_would_be_two_lines() {
        assert_eq!(frame("{\"a\":\"x\\ny\"}").unwrap(), b"{\"a\":\"x\\ny\"}\n");
        assert!(frame("{\"a\":1}\n{\"b\":2}").is_err());
        assert!(frame("a\rb").is_err());
    }
}
