// SPDX-License-Identifier: Apache-2.0
//! Recorded escape sequences replayed against the screens they should
//! leave (FR-032's subset), each worked out by hand.

use super::*;

fn grid(cols: usize, rows: usize, bytes: &[u8]) -> Grid {
    let mut g = Grid::new(cols, rows);
    g.feed(bytes);
    g
}

fn screen(g: &Grid) -> Vec<alloc::string::String> {
    (0..g.rows).map(|y| g.text(y)).collect()
}

#[test]
fn text_wraps_at_the_last_column() {
    let g = grid(5, 3, b"hello world");
    assert_eq!(screen(&g), ["hello", " worl", "d"]);
    assert_eq!((g.x, g.y), (1, 2));
    // The cursor waits past the edge: a CR there doesn't wrap.
    let g = grid(5, 3, b"abcde\rX");
    assert_eq!(screen(&g), ["Xbcde", "", ""]);
    let g = grid(5, 3, b"\x1b[?7labcdefg");
    assert_eq!(g.text(0), "abcdg", "autowrap off: the last column is overwritten");
}

#[test]
fn cursor_moves_and_erases() {
    let g = grid(10, 4, b"\x1b[2;3Hx\x1b[4;10Hy\x1b[H\x1b[3Bz");
    assert_eq!(screen(&g), ["", "  x", "", "z        y"]);
    let g = grid(6, 2, b"abcdef\x1b[1;3H\x1b[K");
    assert_eq!(g.text(0), "ab");
    let g = grid(6, 2, b"abcdef\x1b[1;3H\x1b[1K");
    assert_eq!(g.text(0), "   def");
    let g = grid(6, 3, b"aaaaaa\r\nbbbbbb\r\ncc\x1b[2;3H\x1b[J");
    assert_eq!(screen(&g), ["aaaaaa", "bb", ""]);
    let g = grid(6, 2, b"abcdef\x1b[1;2H\x1b[2X");
    assert_eq!(g.text(0), "a  def", "ECH blanks without moving");
}

#[test]
fn sgr_16_256_and_truecolor() {
    let g = grid(10, 1, b"\x1b[1;31mA\x1b[0mB\x1b[38;5;200mC\x1b[48;2;1;2;3mD\x1b[38:2::4:5:6mE\x1b[97;104mF");
    let l = g.line(0);
    assert_eq!((l[0].attrs & attr::BOLD, l[0].fg), (attr::BOLD, Color::Indexed(1)));
    assert_eq!(l[1], Cell { ch: 'B', ..Cell::default() });
    assert_eq!(l[2].fg, Color::Indexed(200));
    assert_eq!(l[3].bg, Color::Rgb(1, 2, 3));
    assert_eq!(l[4].fg, Color::Rgb(4, 5, 6));
    assert_eq!((l[5].fg, l[5].bg), (Color::Indexed(15), Color::Indexed(12)), "bright colours");
    let g = grid(4, 1, b"\x1b[4;7mU\x1b[24;27mV");
    assert_eq!(g.line(0)[0].attrs, attr::UNDERLINE | attr::INVERSE);
    assert_eq!(g.line(0)[1].attrs, 0);
}

#[test]
fn a_scroll_region_scrolls_only_itself() {
    let g = grid(4, 5, b"0\r\n1\r\n2\r\n3\r\n4\x1b[2;4r\x1b[4;1H\nX");
    assert_eq!(screen(&g), ["0", "2", "3", "X", "4"]);
    assert!(g.scrollback.is_empty(), "a region's lines never reach the scrollback");
    let g = grid(4, 5, b"0\r\n1\r\n2\r\n3\r\n4\x1b[2;4r\x1b[2;1H\x1bMY");
    assert_eq!(screen(&g), ["0", "Y", "1", "2", "4"], "reverse index at the region's top");
}

#[test]
fn the_alternate_screen_keeps_the_main_one() {
    let mut g = grid(6, 2, b"main\x1b[?1049h");
    assert!(g.alternate());
    assert_eq!(screen(&g), ["", ""]);
    g.feed(b"alt");
    g.feed(b"\x1b[?1049l");
    assert!(!g.alternate());
    assert_eq!(screen(&g), ["main", ""]);
    assert_eq!((g.x, g.y), (4, 0), "the cursor is back where it was");
}

#[test]
fn scrollback_keeps_10000_lines() {
    let mut g = Grid::new(8, 3);
    for i in 0..10_010 {
        g.feed(alloc::format!("{i}\r\n").as_bytes());
    }
    assert_eq!(g.scrollback.len(), SCROLLBACK);
    let first: alloc::string::String = g.scrollback[0].iter().map(|c| c.ch).collect();
    assert_eq!(first.trim_end(), "8", "the oldest dropped first");
    // The alternate screen never feeds it.
    let mut a = grid(4, 2, b"\x1b[?1049h");
    a.feed(b"1\r\n2\r\n3\r\n4\r\n");
    assert!(a.scrollback.is_empty());
}

#[test]
fn insert_and_delete_characters_and_lines() {
    let g = grid(6, 1, b"abcdef\x1b[1;2H\x1b[2P");
    assert_eq!(g.text(0), "adef");
    let g = grid(6, 1, b"abcdef\x1b[1;2H\x1b[2@");
    assert_eq!(g.text(0), "a  bcd");
    let g = grid(3, 4, b"a\r\nb\r\nc\r\nd\x1b[2;1H\x1b[L");
    assert_eq!(screen(&g), ["a", "", "b", "c"]);
    let g = grid(3, 4, b"a\r\nb\r\nc\r\nd\x1b[2;1H\x1b[M");
    assert_eq!(screen(&g), ["a", "c", "d", ""]);
}

#[test]
fn modes_reports_and_the_title() {
    let mut g = grid(10, 5, b"\x1b[?2004h\x1b[?1002h\x1b[?1006h\x1b[?25l\x1b[?1h");
    assert!(g.modes.bracketed_paste && g.modes.mouse_sgr && !g.modes.cursor_visible && g.modes.app_cursor);
    assert_eq!(g.modes.mouse, 1002);
    g.feed(b"\x1b[3;7H\x1b[6n");
    assert_eq!(g.replies, b"\x1b[3;7R");
    g.feed(b"\x1b]0;freally:~\x07");
    assert_eq!(g.title, "freally:~");
    g.feed(b"\x1b[?1002l");
    assert_eq!(g.modes.mouse, 0);
}

#[test]
fn tabs_saves_and_reset() {
    let g = grid(20, 1, b"a\tb\tc");
    assert_eq!(g.text(0), "a       b       c");
    let g = grid(10, 3, b"\x1b[2;5H\x1b7\x1b[H\x1b8X");
    assert_eq!(g.text(1), "    X");
    let g = grid(5, 2, b"\x1b[31mhey\x1bc");
    assert_eq!(
        (screen(&g), g.line(0)[0]),
        (alloc::vec![alloc::string::String::new(), alloc::string::String::new()], Cell::default())
    );
}

#[test]
fn erasing_keeps_the_pen_s_background() {
    let g = grid(4, 1, b"\x1b[44m\x1b[2K");
    assert!(g.line(0).iter().all(|c| c.bg == Color::Indexed(4)));
}
