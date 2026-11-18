// SPDX-License-Identifier: Apache-2.0
//! Drawing the grid (TASK-060): each cell's background, its character in
//! Noto Sans Mono on the cell grid, underline and strike lines; the cursor
//! as a block (hollow when the window isn't focused); a selection laid over
//! what it covers. Colours are the theme's: the default pair is the kit's
//! text on its ground, the 16 ANSI colours are tuned to each theme, and the
//! 256-colour cube and greys are xterm's.

use crate::grid::{Cell as GridCell, Color, Grid, attr};
use ffont::atlas::Target;
use ffont::text::{Cell, Face, draw_cell};
use fui::theme::{Mode, Theme, Tok};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub fg: u32,
    pub bg: u32,
    pub cursor: u32,
    pub selection: u32,
    pub ansi: [u32; 16],
}

impl Palette {
    pub fn of(t: &Theme) -> Palette {
        let c = |tok| t.colour(tok).xrgb();
        let ansi = if t.mode == Mode::Light {
            [
                0x111611,
                c(Tok::Error),
                c(Tok::PrimaryInk),
                0xA16207,
                0x1D4ED8,
                0x7E22CE,
                c(Tok::Signal),
                0x545E53,
                0x6B7280,
                0xDC2626,
                0x4D7C0F,
                0xB45309,
                0x2563EB,
                0x9333EA,
                0x0E7490,
                0x1F2937,
            ]
        } else {
            [
                0x28302A,
                c(Tok::Error),
                c(Tok::PrimaryInk),
                c(Tok::Warning),
                0x60A5FA,
                0xC084FC,
                c(Tok::Signal),
                c(Tok::Text2),
                0x6B766E,
                0xFCA5A5,
                0x8CF523,
                0xFDE047,
                0x93C5FD,
                0xD8B4FE,
                0x67E8F9,
                c(Tok::Text),
            ]
        };
        Palette { fg: c(Tok::Text), bg: c(Tok::Bg), cursor: c(Tok::PrimaryInk), selection: c(Tok::Border), ansi }
    }

    /// A cell colour as 0xRRGGBB; `Default` is the fore- or background.
    pub fn rgb(&self, c: Color, fg: bool) -> u32 {
        match c {
            Color::Default => {
                if fg {
                    self.fg
                } else {
                    self.bg
                }
            }
            Color::Indexed(i) if i < 16 => self.ansi[i as usize],
            Color::Indexed(i) if i < 232 => {
                // xterm's 6 × 6 × 6 cube.
                let i = i as u32 - 16;
                let level = |v: u32| if v == 0 { 0 } else { 55 + v * 40 };
                level(i / 36) << 16 | level(i / 6 % 6) << 8 | level(i % 6)
            }
            Color::Indexed(i) => {
                let g = 8 + (i as u32 - 232) * 10;
                g << 16 | g << 8 | g
            }
            Color::Rgb(r, g, b) => (r as u32) << 16 | (g as u32) << 8 | b as u32,
        }
    }

    /// A cell's foreground and background, its attributes applied: bold
    /// brightens the first eight colours, dim fades toward the background,
    /// inverse swaps them.
    pub fn colours(&self, c: &GridCell) -> (u32, u32) {
        let fg_c = match c.fg {
            Color::Indexed(i) if i < 8 && c.attrs & attr::BOLD != 0 => Color::Indexed(i + 8),
            other => other,
        };
        let (mut fg, mut bg) = (self.rgb(fg_c, true), self.rgb(c.bg, false));
        if c.attrs & attr::DIM != 0 {
            fg = mix(fg, bg, 0.5);
        }
        if c.attrs & attr::INVERSE != 0 {
            core::mem::swap(&mut fg, &mut bg);
        }
        (fg, bg)
    }
}

fn mix(a: u32, b: u32, k: f32) -> u32 {
    let ch = |s: u32| {
        let (x, y) = ((a >> s & 0xFF) as f32, (b >> s & 0xFF) as f32);
        ((x + (y - x) * k + 0.5) as u32) << s
    };
    ch(16) | ch(8) | ch(0)
}

/// A selection in grid coordinates: rows counted from the scrollback's
/// oldest line (so it holds while the view scrolls), ends inclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub from: (usize, usize),
    pub to: (usize, usize),
}

impl Selection {
    /// Ordered start and end, (col, row) each.
    pub fn ordered(&self) -> ((usize, usize), (usize, usize)) {
        let (a, b) = (self.from, self.to);
        if (a.1, a.0) <= (b.1, b.0) { (a, b) } else { (b, a) }
    }

    pub fn contains(&self, col: usize, row: usize) -> bool {
        let (a, b) = self.ordered();
        (row > a.1 || (row == a.1 && col >= a.0)) && (row < b.1 || (row == b.1 && col <= b.0))
    }
}

/// What to draw: the grid, how far back into the scrollback the view is
/// (0: the live screen), the selection, and whether the window has focus.
pub struct View<'a> {
    pub grid: &'a Grid,
    pub back: usize,
    pub selection: Option<Selection>,
    pub focused: bool,
}

/// The row of the whole history (scrollback, then screen) the view shows at screen row `y`.
pub fn history_row(grid: &Grid, back: usize, y: usize) -> usize {
    grid.scrollback.len() - back.min(grid.scrollback.len()) + y
}

fn fill(t: &mut impl Target, x: i32, y: i32, w: i32, h: i32, rgb: u32) {
    let (tw, th) = t.size();
    for py in y.max(0)..(y + h).min(th as i32) {
        for px in x.max(0)..(x + w).min(tw as i32) {
            t.set(px as usize, py as usize, rgb);
        }
    }
}

/// Draws the view at (x, y) on `t`, one `cell` per character.
pub fn render(t: &mut impl Target, faces: &mut [Face], px: f32, cell: Cell, (x, y): (i32, i32), v: &View, p: &Palette) {
    let g = v.grid;
    let back = v.back.min(g.scrollback.len());
    for row in 0..g.rows {
        let line: &[GridCell] =
            if row < back { &g.scrollback[g.scrollback.len() - back + row] } else { g.line(row - back) };
        let abs = history_row(g, back, row);
        for col in 0..g.cols {
            let c = line.get(col).copied().unwrap_or_default();
            let (fg, mut bg) = p.colours(&c);
            if v.selection.is_some_and(|s| s.contains(col, abs)) {
                bg = p.selection;
            }
            let (cx, cy) = (x + col as i32 * cell.w, y + row as i32 * cell.h);
            let is_cursor = back == 0 && g.modes.cursor_visible && row == g.y && col == g.x;
            let (fg, bg) = if is_cursor && v.focused { (p.bg, p.cursor) } else { (fg, bg) };
            fill(t, cx, cy, cell.w, cell.h, bg);
            if is_cursor && !v.focused {
                fill(t, cx, cy, cell.w, 1, p.cursor);
                fill(t, cx, cy + cell.h - 1, cell.w, 1, p.cursor);
                fill(t, cx, cy, 1, cell.h, p.cursor);
                fill(t, cx + cell.w - 1, cy, 1, cell.h, p.cursor);
            }
            if c.ch != ' ' {
                draw_cell(t, faces, px, cell, x, y, col as i32, row as i32, c.ch, fg);
            }
            if c.attrs & attr::UNDERLINE != 0 {
                fill(t, cx, cy + cell.baseline + 2, cell.w, 1, fg);
            }
            if c.attrs & attr::STRIKE != 0 {
                fill(t, cx, cy + cell.baseline * 2 / 3, cell.w, 1, fg);
            }
        }
    }
}

/// The selected text, rows joined by newlines, each row's trailing spaces dropped.
pub fn selected_text(g: &Grid, s: Selection) -> alloc::string::String {
    let ((c0, r0), (c1, r1)) = s.ordered();
    let mut out = alloc::string::String::new();
    for row in r0..=r1 {
        let line: &[GridCell] = if row < g.scrollback.len() {
            &g.scrollback[row]
        } else if row - g.scrollback.len() < g.rows {
            g.line(row - g.scrollback.len())
        } else {
            break;
        };
        let from = if row == r0 { c0 } else { 0 };
        let to = if row == r1 { c1 + 1 } else { line.len() };
        let text: alloc::string::String = line[from.min(line.len())..to.min(line.len())].iter().map(|c| c.ch).collect();
        out.push_str(text.trim_end());
        if row != r1 {
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests;
