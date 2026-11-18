// SPDX-License-Identifier: Apache-2.0
//! Drawing, read back pixel by pixel.

use super::*;
use alloc::vec;
use alloc::vec::Vec;
use ffont::text::cell;

struct Canvas {
    w: usize,
    h: usize,
    px: Vec<u32>,
}

impl Target for Canvas {
    fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }
    fn get(&self, x: usize, y: usize) -> u32 {
        self.px[y * self.w + x]
    }
    fn set(&mut self, x: usize, y: usize, rgb: u32) {
        self.px[y * self.w + x] = rgb;
    }
}

const MONO: &[u8] = include_bytes!("../../../../assets/fonts/noto/NotoSansMono-Regular.ttf");

fn draw(g: &Grid, focused: bool, selection: Option<Selection>) -> (Canvas, Cell, Palette) {
    let mut faces = [Face::new(MONO).unwrap()];
    let c = cell(&mut faces[0], 14.0);
    let p = Palette::of(&Theme::new(Mode::Dark, 1.0));
    let mut t = Canvas {
        w: g.cols * c.w as usize,
        h: g.rows * c.h as usize,
        px: vec![0; g.cols * c.w as usize * g.rows * c.h as usize],
    };
    render(&mut t, &mut faces, 14.0, c, (0, 0), &View { grid: g, back: 0, selection, focused }, &p);
    (t, c, p)
}

#[test]
fn colours_from_the_theme_and_xterm() {
    let p = Palette::of(&Theme::new(Mode::Dark, 1.0));
    assert_eq!(p.rgb(Color::Default, true), 0xE8EAF0, "the kit's text");
    assert_eq!(p.rgb(Color::Default, false), 0x090C09, "on the kit's ground");
    assert_eq!(p.rgb(Color::Indexed(2), true), 0x79D920, "green is Freally green");
    assert_eq!(p.rgb(Color::Indexed(16), true), 0x000000);
    assert_eq!(p.rgb(Color::Indexed(196), true), 0xFF0000, "the cube's red");
    assert_eq!(p.rgb(Color::Indexed(231), true), 0xFFFFFF);
    assert_eq!(p.rgb(Color::Indexed(232), true), 0x080808);
    assert_eq!(p.rgb(Color::Indexed(255), true), 0xEEEEEE);
    assert_eq!(p.rgb(Color::Rgb(1, 2, 3), true), 0x010203);
    let bold_red = GridCell { ch: 'x', fg: Color::Indexed(1), bg: Color::Default, attrs: attr::BOLD };
    assert_eq!(p.colours(&bold_red).0, p.ansi[9], "bold brightens");
    let inv = GridCell { attrs: attr::INVERSE, ..GridCell::default() };
    assert_eq!(p.colours(&inv), (p.bg, p.fg));
}

#[test]
fn backgrounds_glyphs_and_the_cursor() {
    let mut g = Grid::new(4, 2);
    g.feed(b"\x1b[41mA\x1b[0m B");
    let (t, c, p) = draw(&g, true, None);
    let at = |col: i32, row: i32, dx: i32, dy: i32| t.px[((row * c.h + dy) * t.w as i32 + col * c.w + dx) as usize];
    assert_eq!(at(0, 0, 0, 0), p.ansi[1], "A's red background, at its corner");
    assert!((0..c.h).any(|dy| (0..c.w).any(|dx| at(0, 0, dx, dy) != p.ansi[1])), "A's glyph is drawn");
    assert!((0..c.h).all(|dy| (0..c.w).all(|dx| at(1, 0, dx, dy) == p.bg)), "a space is only its background");
    // The cursor after B, at column 3: a green block, focused.
    assert_eq!(at(3, 0, 0, 0), p.cursor);
    let (t2, ..) = draw(&g, false, None);
    let at2 = |dx: i32, dy: i32| t2.px[(dy * t2.w as i32 + 3 * c.w + dx) as usize];
    assert_eq!((at2(0, 0), at2(c.w / 2, c.h / 2)), (p.cursor, p.bg), "unfocused: a hollow box");
}

#[test]
fn a_selection_shows_and_copies() {
    let mut g = Grid::new(6, 3);
    g.feed(b"hello\r\nworld!\r\n");
    let sel = Selection { from: (2, 1), to: (1, 0) }; // backwards, from (1,0) to (2,1)
    assert_eq!(selected_text(&g, sel), "ello\nwor");
    let (t, c, p) = draw(&g, true, Some(sel));
    assert_eq!(t.px[c.w as usize], p.selection, "e is selected");
    assert_eq!(t.px[0], p.bg, "h isn't");
}

#[test]
fn the_view_scrolls_into_history() {
    let mut g = Grid::new(4, 2);
    g.feed(b"1\r\n2\r\n3\r\n4");
    assert_eq!(g.scrollback.len(), 2);
    assert_eq!(history_row(&g, 0, 0), 2, "the live screen starts after the history");
    assert_eq!(history_row(&g, 2, 0), 0, "two back: the oldest line on top");
    assert_eq!(selected_text(&g, Selection { from: (0, 0), to: (0, 3) }), "1\n2\n3\n4");
}
