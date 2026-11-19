// SPDX-License-Identifier: Apache-2.0
//! The line on COM2 (PRD § 4): newline-delimited JSON-RPC. Bytes arrive in
//! any pieces; whole lines come out.
//!
//! - A line longer than [`MAX_LINE`] is thrown away up to its newline, never
//!   half-read (the client's request times out; the Brain logs it).
//! - A partial line that stops arriving for [`STALE_MS`] is thrown away (the
//!   bridge reconnected mid-message, PRD § 13): the next line starts clean.

use alloc::vec::Vec;

/// The longest line read: a swap's binary in base64 fits with room to spare.
pub const MAX_LINE: usize = 32 << 20;
/// A partial line silent this long is dropped.
pub const STALE_MS: u64 = 2_000;

/// Gathers bytes into lines.
#[derive(Default)]
pub struct Lines {
    buf: Vec<u8>,
    /// Throwing away an overlong line, until its newline.
    skipping: bool,
    /// When the partial line last grew.
    last_ms: u64,
    /// Lines thrown away (too long, or stale), for the log.
    pub dropped: u32,
}

impl Lines {
    /// Bytes that arrived at `now_ms`; the whole lines they finished.
    pub fn push(&mut self, bytes: &[u8], now_ms: u64) -> Vec<Vec<u8>> {
        self.expire(now_ms);
        self.last_ms = now_ms;
        let mut out = Vec::new();
        for &b in bytes {
            if b == b'\n' {
                if self.skipping {
                    self.skipping = false;
                } else {
                    out.push(core::mem::take(&mut self.buf));
                }
                continue;
            }
            if self.skipping {
                continue;
            }
            if self.buf.len() == MAX_LINE {
                self.buf = Vec::new();
                self.skipping = true;
                self.dropped += 1;
                continue;
            }
            self.buf.push(b);
        }
        out
    }

    /// Drops a partial line that has been silent for [`STALE_MS`].
    pub fn expire(&mut self, now_ms: u64) {
        if (!self.buf.is_empty() || self.skipping) && now_ms.saturating_sub(self.last_ms) >= STALE_MS {
            if !self.buf.is_empty() {
                self.dropped += 1;
            }
            self.buf = Vec::new();
            self.skipping = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_come_whole_whatever_the_pieces() {
        let mut l = Lines::default();
        assert!(l.push(b"{\"a\":", 0).is_empty());
        assert_eq!(l.push(b"1}\n{\"b\"", 10), [b"{\"a\":1}".to_vec()]);
        assert_eq!(l.push(b":2}\n\n", 20), [b"{\"b\":2}".to_vec(), Vec::new()]);
    }

    #[test]
    fn a_stale_partial_line_is_dropped() {
        let mut l = Lines::default();
        assert!(l.push(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"me", 0).is_empty());
        // The bridge reconnected; a whole request follows 3 s later.
        assert_eq!(l.push(b"{\"id\":2}\n", 3_000), [b"{\"id\":2}".to_vec()]);
        assert_eq!(l.dropped, 1);
        // A slow line that keeps coming is kept.
        assert!(l.push(b"{\"id\":", 4_000).is_empty());
        assert_eq!(l.push(b"3}\n", 5_500), [b"{\"id\":3}".to_vec()]);
    }

    #[test]
    fn an_overlong_line_is_skipped_to_its_end() {
        let mut l = Lines::default();
        let big = alloc::vec![b'x'; MAX_LINE + 5];
        assert!(l.push(&big, 0).is_empty());
        assert_eq!(l.push(b"yyy\n{\"id\":4}\n", 1), [b"{\"id\":4}".to_vec()]);
        assert_eq!(l.dropped, 1);
    }
}
