// SPDX-License-Identifier: Apache-2.0
//! Line editing (TASK-061, FR-033): the bytes a terminal sends for keys in,
//! the line out when Enter goes, and what to draw meanwhile. Arrows, Home,
//! End and Delete as xterm sends them (`ESC [ D`, `ESC O H`, `ESC [ 3 ~`…);
//! Ctrl+A, E, U, K, W, L and C; Up and Down walk the history (the last 500
//! lines, the one being typed kept aside while walking).

use alloc::string::String;
use alloc::vec::Vec;

pub const HISTORY: usize = 500;

/// What a byte (or a sequence) did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Nothing to show yet (inside a sequence).
    Nothing,
    /// The line changed: draw it again.
    Redraw,
    /// Enter: the line, done.
    Line(String),
    /// Ctrl+C: the line abandoned.
    Cancel,
    /// Ctrl+L: clear the screen.
    Clear,
}

#[derive(Debug, Default)]
pub struct Editor {
    buf: Vec<char>,
    cursor: usize,
    history: Vec<String>,
    /// Walking the history: which line (from the newest), and what was being typed.
    walk: Option<(usize, Vec<char>)>,
    /// An escape sequence in progress.
    esc: Vec<u8>,
}

impl Editor {
    pub fn new() -> Editor {
        Editor::default()
    }

    pub fn line(&self) -> String {
        self.buf.iter().collect()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Feeds bytes; every event they make, in order.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        let mut out = Vec::new();
        let text = String::from_utf8_lossy(bytes);
        for c in text.chars() {
            let e = self.char(c);
            if e != Event::Nothing {
                out.push(e);
            }
        }
        out
    }

    fn char(&mut self, c: char) -> Event {
        if !self.esc.is_empty() {
            self.esc.push(c as u8);
            return self.sequence();
        }
        match c {
            '\x1b' => {
                self.esc.push(0x1B);
                Event::Nothing
            }
            '\r' | '\n' => {
                let line = self.line();
                if !line.trim().is_empty() && self.history.last() != Some(&line) {
                    if self.history.len() == HISTORY {
                        self.history.remove(0);
                    }
                    self.history.push(line.clone());
                }
                self.buf.clear();
                self.cursor = 0;
                self.walk = None;
                Event::Line(line)
            }
            '\x7f' | '\x08' => {
                if self.cursor == 0 {
                    return Event::Nothing;
                }
                self.cursor -= 1;
                self.buf.remove(self.cursor);
                Event::Redraw
            }
            '\x01' => self.go(0),
            '\x05' => self.go(self.buf.len()),
            '\x03' => {
                self.buf.clear();
                self.cursor = 0;
                self.walk = None;
                Event::Cancel
            }
            '\x0c' => Event::Clear,
            '\x15' => {
                self.buf.drain(..self.cursor);
                self.cursor = 0;
                Event::Redraw
            }
            '\x0b' => {
                self.buf.truncate(self.cursor);
                Event::Redraw
            }
            '\x17' => {
                // The word before the cursor, and the spaces after it.
                let mut i = self.cursor;
                while i > 0 && self.buf[i - 1] == ' ' {
                    i -= 1;
                }
                while i > 0 && self.buf[i - 1] != ' ' {
                    i -= 1;
                }
                self.buf.drain(i..self.cursor);
                self.cursor = i;
                Event::Redraw
            }
            c if c.is_control() => Event::Nothing,
            c => {
                if self.buf.len() < 4096 {
                    self.buf.insert(self.cursor, c);
                    self.cursor += 1;
                }
                Event::Redraw
            }
        }
    }

    fn go(&mut self, to: usize) -> Event {
        self.cursor = to.min(self.buf.len());
        Event::Redraw
    }

    /// An escape sequence, once it's complete (`ESC O x`, or `ESC [` up to
    /// a letter or `~`; anything else, or one too long, is dropped).
    fn sequence(&mut self) -> Event {
        let s = &self.esc[1..];
        let complete = match s {
            [] | [b'[' | b'O'] => false,
            [b'O', _] => true,
            [b'[', .., last] => last.is_ascii_alphabetic() || *last == b'~' || s.len() > 8,
            _ => true,
        };
        if !complete {
            return Event::Nothing;
        }
        let done = core::mem::take(&mut self.esc);
        match &done[1..] {
            b"[D" | b"OD" => self.go(self.cursor.saturating_sub(1)),
            b"[C" | b"OC" => self.go(self.cursor + 1),
            b"[H" | b"OH" | b"[1~" => self.go(0),
            b"[F" | b"OF" | b"[4~" => self.go(self.buf.len()),
            b"[3~" => {
                if self.cursor < self.buf.len() {
                    self.buf.remove(self.cursor);
                    Event::Redraw
                } else {
                    Event::Nothing
                }
            }
            b"[A" | b"OA" => self.older(),
            b"[B" | b"OB" => self.newer(),
            _ => Event::Nothing,
        }
    }

    fn older(&mut self) -> Event {
        let next = match &self.walk {
            None if self.history.is_empty() => return Event::Nothing,
            None => 0,
            Some((i, _)) if i + 1 < self.history.len() => i + 1,
            Some(_) => return Event::Nothing,
        };
        if self.walk.is_none() {
            self.walk = Some((0, self.buf.clone()));
        }
        if let Some((i, _)) = &mut self.walk {
            *i = next;
        }
        self.buf = self.history[self.history.len() - 1 - next].chars().collect();
        self.cursor = self.buf.len();
        Event::Redraw
    }

    fn newer(&mut self) -> Event {
        match self.walk.take() {
            None => Event::Nothing,
            Some((0, typed)) => {
                self.buf = typed;
                self.cursor = self.buf.len();
                Event::Redraw
            }
            Some((i, typed)) => {
                self.buf = self.history[self.history.len() - i].chars().collect();
                self.cursor = self.buf.len();
                self.walk = Some((i - 1, typed));
                Event::Redraw
            }
        }
    }

    /// The bytes that draw `prompt` and the line on the current row, the
    /// cursor where it belongs.
    pub fn draw(&self, prompt: &str) -> String {
        let line = self.line();
        let back = self.buf.len() - self.cursor;
        let mut s = alloc::format!("\r\x1b[K{prompt}{line}");
        if back > 0 {
            s.push_str(&alloc::format!("\x1b[{back}D"));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn typed(e: &mut Editor, s: &str) {
        e.feed(s.as_bytes());
    }

    #[test]
    fn typing_moving_and_deleting() {
        let mut e = Editor::new();
        typed(&mut e, "helo");
        typed(&mut e, "\x1b[D");
        typed(&mut e, "l");
        assert_eq!((e.line().as_str(), e.cursor()), ("hello", 4));
        typed(&mut e, "\x1bOH\x1b[3~");
        assert_eq!(e.line(), "ello", "Home, then Delete");
        typed(&mut e, "\x05\x7f");
        assert_eq!(e.line(), "ell", "End, then Backspace");
        typed(&mut e, "\x01\x0b");
        assert_eq!(e.line(), "", "Ctrl+A, Ctrl+K");
    }

    #[test]
    fn enter_gives_the_line_and_keeps_it() {
        let mut e = Editor::new();
        assert_eq!(e.feed(b"ps\r"), vec![Event::Redraw, Event::Redraw, Event::Line("ps".into())]);
        assert_eq!(e.line(), "");
        e.feed(b"ps\r   \r");
        assert_eq!(e.history(), ["ps"], "no repeat, no blank line");
    }

    #[test]
    fn up_and_down_walk_the_history_keeping_what_was_typed() {
        let mut e = Editor::new();
        e.feed(b"boot\rmem\r");
        typed(&mut e, "ver");
        typed(&mut e, "\x1b[A");
        assert_eq!(e.line(), "mem");
        typed(&mut e, "\x1b[A");
        assert_eq!(e.line(), "boot");
        assert_eq!(e.feed(b"\x1b[A"), vec![], "nothing older");
        typed(&mut e, "\x1b[B");
        assert_eq!(e.line(), "mem");
        typed(&mut e, "\x1b[B");
        assert_eq!(e.line(), "ver", "back to what was being typed");
    }

    #[test]
    fn ctrl_keys() {
        let mut e = Editor::new();
        typed(&mut e, "log stage now");
        typed(&mut e, "\x17");
        assert_eq!(e.line(), "log stage ");
        typed(&mut e, "\x17");
        assert_eq!(e.line(), "log ");
        typed(&mut e, "x\x01\x1b[C\x15");
        assert_eq!(e.line(), "og x", "Ctrl+U takes what's before the cursor");
        assert_eq!(e.feed(b"\x03"), vec![Event::Cancel]);
        assert_eq!(e.line(), "");
        assert_eq!(e.feed(b"\x0c"), vec![Event::Clear]);
    }

    #[test]
    fn drawing_puts_the_cursor_back() {
        let mut e = Editor::new();
        typed(&mut e, "abc\x1b[D\x1b[D");
        assert_eq!(e.draw("$ "), "\r\x1b[K$ abc\x1b[2D");
        typed(&mut e, "\x05");
        assert_eq!(e.draw("$ "), "\r\x1b[K$ abc");
    }

    #[test]
    fn unicode_and_a_long_history() {
        let mut e = Editor::new();
        typed(&mut e, "héllo ✓\x1b[D\x7f");
        assert_eq!(e.line(), "héllo✓");
        for i in 0..HISTORY + 20 {
            e.feed(alloc::format!("c{i}\r").as_bytes());
        }
        assert_eq!(e.history().len(), HISTORY);
        assert_eq!(e.history()[0], "c20");
    }
}
