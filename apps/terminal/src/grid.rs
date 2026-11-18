// SPDX-License-Identifier: Apache-2.0
//! The screen a terminal keeps (TASK-060, FR-032): a grid of cells (a
//! character, its colours, its attributes), the cursor, the scroll region,
//! the alternate screen, the modes, and 10,000 lines of scrollback. The
//! parser's actions are carried out here; what the program asks back (a
//! cursor report) waits in `replies`.

use crate::vt::{Action, Param, Parser, count};
use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;

pub const SCROLLBACK: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Default,
    /// 0–15 the ANSI colours (8–15 bright), 16–255 xterm's cube and greys.
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// Attribute bits.
pub mod attr {
    pub const BOLD: u8 = 1 << 0;
    pub const DIM: u8 = 1 << 1;
    pub const ITALIC: u8 = 1 << 2;
    pub const UNDERLINE: u8 = 1 << 3;
    pub const INVERSE: u8 = 1 << 4;
    pub const STRIKE: u8 = 1 << 5;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub attrs: u8,
}

impl Default for Cell {
    fn default() -> Cell {
        Cell { ch: ' ', fg: Color::Default, bg: Color::Default, attrs: 0 }
    }
}

/// What the program has turned on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Modes {
    pub autowrap: bool,
    pub cursor_visible: bool,
    /// Arrow keys as `ESC O A` (DECCKM).
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    /// Mouse reporting: 1000 (presses), 1002 (drags too), 1003 (all moves).
    pub mouse: u16,
    /// Reports in SGR form (1006).
    pub mouse_sgr: bool,
}

impl Default for Modes {
    fn default() -> Modes {
        Modes {
            autowrap: true,
            cursor_visible: true,
            app_cursor: false,
            bracketed_paste: false,
            mouse: 0,
            mouse_sgr: false,
        }
    }
}

#[derive(Clone, Debug)]
struct Saved {
    x: usize,
    y: usize,
    pen: Cell,
}

#[derive(Debug)]
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    lines: Vec<Vec<Cell>>,
    /// The main screen, kept while the alternate one shows.
    main: Option<Vec<Vec<Cell>>>,
    pub scrollback: VecDeque<Vec<Cell>>,
    pub x: usize,
    pub y: usize,
    /// The next character wraps first (the cursor sits past the last column).
    wrap_next: bool,
    pen: Cell,
    top: usize,
    bottom: usize,
    saved: Option<Saved>,
    pub modes: Modes,
    pub title: alloc::string::String,
    /// Bytes to send back to the program (reports).
    pub replies: Vec<u8>,
    parser: Parser,
}

impl Grid {
    pub fn new(cols: usize, rows: usize) -> Grid {
        let (cols, rows) = (cols.max(1), rows.max(1));
        Grid {
            cols,
            rows,
            lines: vec![vec![Cell::default(); cols]; rows],
            main: None,
            scrollback: VecDeque::new(),
            x: 0,
            y: 0,
            wrap_next: false,
            pen: Cell::default(),
            top: 0,
            bottom: rows - 1,
            saved: None,
            modes: Modes::default(),
            title: alloc::string::String::new(),
            replies: Vec::new(),
            parser: Parser::new(),
        }
    }

    pub fn line(&self, y: usize) -> &[Cell] {
        &self.lines[y]
    }

    pub fn alternate(&self) -> bool {
        self.main.is_some()
    }

    /// A row's characters, trailing spaces dropped (for tests and copying).
    pub fn text(&self, y: usize) -> alloc::string::String {
        let s: alloc::string::String = self.lines[y].iter().map(|c| c.ch).collect();
        alloc::string::String::from(s.trim_end())
    }

    /// Feeds the program's output.
    pub fn feed(&mut self, bytes: &[u8]) {
        let mut parser = core::mem::take(&mut self.parser);
        parser.feed(bytes, &mut |a| self.apply(a));
        self.parser = parser;
    }

    fn blank(&self) -> Cell {
        // Erased cells keep the pen's background, as xterm does.
        Cell { ch: ' ', fg: Color::Default, bg: self.pen.bg, attrs: 0 }
    }

    /// Scrolls the region up by `n`: its top lines go, into the scrollback
    /// when `history` (text running off the top of the main screen; never a
    /// scroll the program asks for, nor the alternate screen).
    fn scroll_up(&mut self, n: usize, history: bool) {
        for _ in 0..n.min(self.bottom - self.top + 1) {
            let gone = self.lines.remove(self.top);
            if history && self.top == 0 && self.main.is_none() {
                if self.scrollback.len() == SCROLLBACK {
                    self.scrollback.pop_front();
                }
                self.scrollback.push_back(gone);
            }
            let blank = vec![self.blank(); self.cols];
            self.lines.insert(self.bottom, blank);
        }
    }

    fn scroll_down(&mut self, n: usize) {
        for _ in 0..n.min(self.bottom - self.top + 1) {
            self.lines.remove(self.bottom);
            let blank = vec![self.blank(); self.cols];
            self.lines.insert(self.top, blank);
        }
    }

    fn newline(&mut self) {
        if self.y == self.bottom {
            self.scroll_up(1, true);
        } else if self.y + 1 < self.rows {
            self.y += 1;
        }
    }

    fn print(&mut self, ch: char) {
        if self.wrap_next {
            self.wrap_next = false;
            if self.modes.autowrap {
                self.x = 0;
                self.newline();
            }
        }
        let cell = Cell { ch, ..self.pen };
        self.lines[self.y][self.x] = cell;
        if self.x + 1 == self.cols {
            self.wrap_next = true;
        } else {
            self.x += 1;
        }
    }

    fn goto(&mut self, x: usize, y: usize) {
        self.x = x.min(self.cols - 1);
        self.y = y.min(self.rows - 1);
        self.wrap_next = false;
    }

    fn erase(&mut self, y: usize, from: usize, to: usize) {
        let b = self.blank();
        for c in &mut self.lines[y][from.min(self.cols)..to.min(self.cols)] {
            *c = b;
        }
    }

    pub fn apply(&mut self, a: Action) {
        match a {
            Action::Print(c) => self.print(c),
            Action::Execute(b) => match b {
                0x08 => {
                    self.x = self.x.saturating_sub(1);
                    self.wrap_next = false;
                }
                0x09 => {
                    let next = ((self.x / 8) + 1) * 8;
                    self.x = next.min(self.cols - 1);
                }
                0x0A..=0x0C => self.newline(),
                0x0D => {
                    self.x = 0;
                    self.wrap_next = false;
                }
                _ => {}
            },
            Action::Esc { intermediates, final_byte } => {
                if !intermediates.is_empty() {
                    return; // character sets: UTF-8 only
                }
                match final_byte {
                    b'7' => self.save(),
                    b'8' => self.restore(),
                    b'D' => self.newline(),
                    b'E' => {
                        self.x = 0;
                        self.newline();
                    }
                    b'M' => {
                        if self.y == self.top {
                            self.scroll_down(1);
                        } else {
                            self.y = self.y.saturating_sub(1);
                        }
                    }
                    b'c' => {
                        let (c, r) = (self.cols, self.rows);
                        *self = Grid::new(c, r);
                    }
                    _ => {}
                }
            }
            Action::Csi { private, params, intermediates, final_byte } => {
                if intermediates.is_empty() {
                    self.csi(private, &params, final_byte);
                }
            }
            Action::Osc(bytes) => {
                // `0;` and `2;` set the title.
                if let Some(t) = bytes.strip_prefix(b"0;").or_else(|| bytes.strip_prefix(b"2;")) {
                    self.title = alloc::string::String::from_utf8_lossy(t).chars().take(256).collect();
                }
            }
        }
    }

    fn save(&mut self) {
        self.saved = Some(Saved { x: self.x, y: self.y, pen: self.pen });
    }

    fn restore(&mut self) {
        if let Some(s) = self.saved.clone() {
            self.goto(s.x, s.y);
            self.pen = s.pen;
        }
    }

    fn csi(&mut self, private: Option<u8>, p: &[Param], f: u8) {
        if private == Some(b'?') {
            let on = f == b'h';
            if f == b'h' || f == b'l' {
                for m in p {
                    self.dec_mode(m.value.unwrap_or(0), on);
                }
            }
            return;
        }
        if private.is_some() {
            return;
        }
        let n = count(p, 0, 1) as usize;
        match f {
            b'A' => self.goto(self.x, self.y.saturating_sub(n).max(if self.y >= self.top { self.top } else { 0 })),
            b'B' => {
                let limit = if self.y <= self.bottom { self.bottom } else { self.rows - 1 };
                self.goto(self.x, (self.y + n).min(limit))
            }
            b'C' => self.goto(self.x + n, self.y),
            b'D' => self.goto(self.x.saturating_sub(n), self.y),
            b'E' => self.goto(0, self.y + n),
            b'F' => self.goto(0, self.y.saturating_sub(n)),
            b'G' | b'`' => self.goto(n - 1, self.y),
            b'd' => self.goto(self.x, n - 1),
            b'H' | b'f' => self.goto(count(p, 1, 1) as usize - 1, n - 1),
            b'J' => {
                let (x, y) = (self.x, self.y);
                match p.first().and_then(|q| q.value).unwrap_or(0) {
                    0 => {
                        self.erase(y, x, self.cols);
                        for r in y + 1..self.rows {
                            self.erase(r, 0, self.cols);
                        }
                    }
                    1 => {
                        for r in 0..y {
                            self.erase(r, 0, self.cols);
                        }
                        self.erase(y, 0, x + 1);
                    }
                    2 => (0..self.rows).for_each(|r| self.erase(r, 0, self.cols)),
                    3 => self.scrollback.clear(),
                    _ => {}
                }
            }
            b'K' => {
                let (x, y) = (self.x, self.y);
                match p.first().and_then(|q| q.value).unwrap_or(0) {
                    0 => self.erase(y, x, self.cols),
                    1 => self.erase(y, 0, x + 1),
                    2 => self.erase(y, 0, self.cols),
                    _ => {}
                }
            }
            b'X' => {
                let (x, y) = (self.x, self.y);
                self.erase(y, x, x + n);
            }
            b'P' => {
                let (x, y, b) = (self.x, self.y, self.blank());
                let row = &mut self.lines[y];
                for _ in 0..n.min(self.cols - x) {
                    row.remove(x);
                    row.push(b);
                }
            }
            b'@' => {
                let (x, y, b) = (self.x, self.y, self.blank());
                let row = &mut self.lines[y];
                for _ in 0..n.min(self.cols - x) {
                    row.pop();
                    row.insert(x, b);
                }
            }
            b'L' | b'M' if (self.top..=self.bottom).contains(&self.y) => {
                let saved_top = self.top;
                self.top = self.y;
                if f == b'L' {
                    self.scroll_down(n)
                } else {
                    self.scroll_up(n, false)
                }
                self.top = saved_top;
                self.x = 0;
            }
            b'S' => self.scroll_up(n, false),
            b'T' => self.scroll_down(n),
            b'r' => {
                let top = count(p, 0, 1) as usize - 1;
                let bottom = (count(p, 1, self.rows as u32) as usize - 1).min(self.rows - 1);
                if top < bottom {
                    self.top = top;
                    self.bottom = bottom;
                    self.goto(0, 0);
                }
            }
            b's' => self.save(),
            b'u' => self.restore(),
            b'm' => self.sgr(p),
            b'n' if p.first().and_then(|q| q.value) == Some(6) => {
                let r = alloc::format!("\x1b[{};{}R", self.y + 1, self.x + 1);
                self.replies.extend_from_slice(r.as_bytes());
            }
            _ => {}
        }
    }

    fn dec_mode(&mut self, mode: u32, on: bool) {
        match mode {
            1 => self.modes.app_cursor = on,
            7 => self.modes.autowrap = on,
            25 => self.modes.cursor_visible = on,
            1000 | 1002 | 1003 => self.modes.mouse = if on { mode as u16 } else { 0 },
            1006 => self.modes.mouse_sgr = on,
            2004 => self.modes.bracketed_paste = on,
            1049 | 1047 | 47 => {
                if on && self.main.is_none() {
                    if mode == 1049 {
                        self.save();
                    }
                    let alt = vec![vec![Cell::default(); self.cols]; self.rows];
                    self.main = Some(core::mem::replace(&mut self.lines, alt));
                    self.goto(0, 0);
                } else if !on && let Some(main) = self.main.take() {
                    self.lines = main;
                    if mode == 1049 {
                        self.restore();
                    }
                }
            }
            _ => {}
        }
    }

    fn sgr(&mut self, p: &[Param]) {
        if p.is_empty() {
            self.pen = Cell::default();
            return;
        }
        let mut i = 0;
        while i < p.len() {
            let v = p[i].value.unwrap_or(0);
            match v {
                0 => self.pen = Cell::default(),
                1 => self.pen.attrs |= attr::BOLD,
                2 => self.pen.attrs |= attr::DIM,
                3 => self.pen.attrs |= attr::ITALIC,
                4 => self.pen.attrs |= attr::UNDERLINE,
                7 => self.pen.attrs |= attr::INVERSE,
                9 => self.pen.attrs |= attr::STRIKE,
                22 => self.pen.attrs &= !(attr::BOLD | attr::DIM),
                23 => self.pen.attrs &= !attr::ITALIC,
                24 => self.pen.attrs &= !attr::UNDERLINE,
                27 => self.pen.attrs &= !attr::INVERSE,
                29 => self.pen.attrs &= !attr::STRIKE,
                30..=37 => self.pen.fg = Color::Indexed((v - 30) as u8),
                39 => self.pen.fg = Color::Default,
                40..=47 => self.pen.bg = Color::Indexed((v - 40) as u8),
                49 => self.pen.bg = Color::Default,
                90..=97 => self.pen.fg = Color::Indexed((v - 90 + 8) as u8),
                100..=107 => self.pen.bg = Color::Indexed((v - 100 + 8) as u8),
                38 | 48 => {
                    // `38;5;n`, `38;2;r;g;b`, or their `:` forms.
                    let (c, used) = extended(&p[i..]);
                    if let Some(c) = c {
                        if v == 38 {
                            self.pen.fg = c;
                        } else {
                            self.pen.bg = c;
                        }
                    }
                    i += used;
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
    }
}

/// An extended colour at the start of `p` (`38`/`48` and what follows), and
/// how many parameters it took.
fn extended(p: &[Param]) -> (Option<Color>, usize) {
    let byte = |v: Option<u32>| v.map(|v| v.min(255) as u8);
    if !p[0].sub.is_empty() {
        // The colon form: 38:5:n, 38:2::r:g:b (or 38:2:r:g:b).
        let s = &p[0].sub;
        return match s.first().copied().flatten() {
            Some(5) => (byte(s.get(1).copied().flatten()).map(Color::Indexed), 1),
            Some(2) => {
                let rgb: Vec<Option<u32>> = if s.len() >= 5 { s[2..5].to_vec() } else { s[1..].to_vec() };
                match (rgb.first().copied().flatten(), rgb.get(1).copied().flatten(), rgb.get(2).copied().flatten()) {
                    (Some(r), Some(g), Some(b)) => {
                        (Some(Color::Rgb(r.min(255) as u8, g.min(255) as u8, b.min(255) as u8)), 1)
                    }
                    _ => (None, 1),
                }
            }
            _ => (None, 1),
        };
    }
    match p.get(1).and_then(|q| q.value) {
        Some(5) => (p.get(2).and_then(|q| byte(q.value)).map(Color::Indexed), 3),
        Some(2) => {
            let c = |i: usize| p.get(i).and_then(|q| byte(q.value));
            match (c(2), c(3), c(4)) {
                (Some(r), Some(g), Some(b)) => (Some(Color::Rgb(r, g, b)), 5),
                _ => (None, p.len().min(5)),
            }
        }
        _ => (None, 2.min(p.len())),
    }
}

#[cfg(test)]
mod tests;
