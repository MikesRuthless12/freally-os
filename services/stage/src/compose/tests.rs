// SPDX-License-Identifier: Apache-2.0
//! Composing, read back pixel by pixel on a 400 × 300 screen.

use super::*;
use fui::text::FontData;
use fui::theme::Mode;
use fwire::proto::stage::Kind;

fn fonts() -> Fonts<'static> {
    Fonts::new(FontData {
        regular: include_bytes!("../../../../assets/fonts/noto/NotoSans-Regular.ttf"),
        medium: include_bytes!("../../../../assets/fonts/noto/NotoSans-Medium.ttf"),
        semibold: include_bytes!("../../../../assets/fonts/noto/NotoSans-SemiBold.ttf"),
        mono: include_bytes!("../../../../assets/fonts/noto/NotoSansMono-Regular.ttf"),
        symbols: include_bytes!("../../../../assets/fonts/noto/NotoSansSymbols2-Regular.ttf"),
    })
    .unwrap()
}

const ME: (u32, &str) = (1, "t");

/// One normal window, 200 × 100 (frame (100, 84) 200 × 132), its surface
/// solid `rgb` (opaque), composed whole in `mode`.
fn one(mode: Mode, rgb: u32) -> (Back, Windows, Theme) {
    let mut ws = Windows::new(400, 300, 32);
    ws.create("Terminal", Kind::Normal, (200, 100), (0, 0), (0, 0), ME);
    let px: &'static [u32] = alloc::vec![0xFF00_0000 | rgb; 200 * 100].leak();
    let surface: &'static (u32, u32, &[u32]) = alloc::boxed::Box::leak(alloc::boxed::Box::new((200, 100, px)));
    let mut back = Back::new(400, 300);
    let theme = Theme::new(mode, 1.0);
    let mut f = fonts();
    let mut look = Look { theme, fonts: &mut f, art: None };
    let all = back.rect();
    compose(&mut back, all, &ws, &|_| Some(surface as &dyn Pixels), &mut look);
    (back, ws, theme)
}

fn at(b: &Back, x: u32, y: u32) -> u32 {
    b.px[(y * b.w + x) as usize]
}

#[test]
fn the_ground_the_strip_the_surface_and_the_border() {
    let (b, ws, t) = one(Mode::Dark, 0x123456);
    let w = &ws.stack()[0];
    assert_eq!(w.frame, Rect::new(100, 84, 200, 132));
    assert_eq!(at(&b, 5, 5), t.colour(Tok::Bg).xrgb(), "the ground");
    assert_eq!(at(&b, 150, 120), 0x123456, "the surface");
    assert_eq!(at(&b, 105, 100), t.colour(Tok::Surface2).xrgb(), "the focused strip");
    assert_eq!(at(&b, 100, 150), t.colour(Tok::Border).xrgb(), "the left border, mid-side");
    assert_eq!(at(&b, 299, 150), t.colour(Tok::Border).xrgb(), "the right border");
    assert_eq!(at(&b, 100, 84), t.colour(Tok::Bg).xrgb(), "outside the rounded corner");
    // On the curve: part border, part ground.
    let corner = at(&b, 102, 87);
    assert!(corner != t.colour(Tok::Bg).xrgb() && corner != t.colour(Tok::Border).xrgb(), "{corner:06x}");
}

#[test]
fn the_title_and_the_close_cross_are_drawn_in_the_strip() {
    let (b, _, t) = one(Mode::Dark, 0);
    let strip = t.colour(Tok::Surface2).xrgb();
    let inked = |x0: u32, x1: u32| {
        // Inside the border (the strip's top row is the border).
        (86..114).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| at(&b, x, y) != strip).count()
    };
    assert!(inked(112, 180) > 40, "the title's words");
    assert!(inked(270, 298) > 10, "the ×");
    assert_eq!(inked(200, 260), 0, "nothing between");
}

#[test]
fn an_unfocused_window_wears_the_plain_strip() {
    let mut ws = Windows::new(400, 300, 32);
    let a = ws.create("a", Kind::Normal, (200, 100), (0, 0), (0, 0), ME);
    ws.create("b", Kind::Normal, (100, 50), (0, 0), (0, 0), ME);
    ws.move_to(ws.stack()[1].id, 0, 250);
    let mut back = Back::new(400, 300);
    let theme = Theme::new(Mode::Dark, 1.0);
    let mut f = fonts();
    let all = back.rect();
    compose(&mut back, all, &ws, &|_| None, &mut Look { theme, fonts: &mut f, art: None });
    let _ = a;
    assert_eq!(at(&back, 250, 90), theme.colour(Tok::Surface).xrgb(), "a lost focus to b");
    assert_eq!(at(&back, 150, 150), theme.colour(Tok::Surface).xrgb(), "no pixels yet: the surface colour");
}

#[test]
fn the_light_theme_is_composed_from_its_own_tokens() {
    let (dark, ..) = one(Mode::Dark, 0x123456);
    let (light, _, t) = one(Mode::Light, 0x123456);
    assert_eq!(at(&light, 5, 5), t.colour(Tok::Bg).xrgb());
    assert_eq!(at(&light, 105, 100), t.colour(Tok::Surface2).xrgb());
    assert_ne!(at(&light, 105, 100), at(&dark, 105, 100), "the strip follows the theme");
    assert_eq!(at(&light, 150, 120), 0x123456, "the surface is the client's");
}

#[test]
fn translucent_pixels_lie_over_the_window_s_surface_colour() {
    let mut ws = Windows::new(100, 100, 32);
    ws.create("a", Kind::Normal, (50, 50), (0, 0), (0, 0), ME);
    // White at half: premultiplied 0x80808080.
    let px: &'static [u32] = alloc::vec![0x8080_8080u32; 50 * 50].leak();
    let s = (50u32, 50u32, px);
    let mut back = Back::new(100, 100);
    let theme = Theme::new(Mode::Dark, 1.0);
    let mut f = fonts();
    let all = back.rect();
    compose(&mut back, all, &ws, &|_| Some(&s as &dyn Pixels), &mut Look { theme, fonts: &mut f, art: None });
    let base = theme.colour(Tok::Surface).xrgb();
    assert_eq!(at(&back, 50, 60), over(base, 0x8080_8080));
}

#[test]
fn only_the_damaged_area_is_composed() {
    let ws = Windows::new(100, 100, 32);
    let mut back = Back::new(100, 100);
    back.px.fill(0xABCDEF);
    let theme = Theme::new(Mode::Dark, 1.0);
    let mut f = fonts();
    compose(&mut back, Rect::new(10, 10, 5, 5), &ws, &|_| None, &mut Look { theme, fonts: &mut f, art: None });
    assert_eq!(at(&back, 12, 12), theme.colour(Tok::Bg).xrgb());
    assert_eq!(at(&back, 9, 12), 0xABCDEF, "left of it");
    assert_eq!(at(&back, 15, 12), 0xABCDEF, "right of it");
    assert_eq!(back.px.iter().filter(|&&p| p != 0xABCDEF).count(), 25);
}

#[test]
fn over_and_long_titles() {
    assert_eq!(over(0x0000FF, 0xFF00FF00), 0x00FF00, "opaque replaces");
    assert_eq!(over(0x123456, 0), 0x123456, "transparent keeps");
    assert_eq!(over(0x0000FF, 0x8080_0000), 0x80007F, "half red over blue");
    let mut f = fonts();
    let t = Theme::new(Mode::Dark, 1.0);
    let cut = fit(&mut f, t, "A very long window title that won't fit", 120);
    assert!(cut.ends_with('…') && f.measure(t, TextStyle::Body, &cut).0 <= 120, "{cut}");
    assert_eq!(fit(&mut f, t, "Short", 120), "Short");
}

#[test]
fn present_copies_only_the_area_and_swaps_for_rgbx() {
    let mut back = Back::new(4, 2);
    back.px.copy_from_slice(&[0x112233, 0x445566, 0x778899, 0xAABBCC, 1, 2, 3, 4]);
    let mut fb = alloc::vec![0u32; 6 * 2];
    // SAFETY: fb is 2 rows of stride 6.
    unsafe { present(&back, Rect::new(1, 0, 2, 2), fb.as_mut_ptr(), 6, 0) };
    assert_eq!(fb, alloc::vec![0, 0x445566, 0x778899, 0, 0, 0, 0, 2, 3, 0, 0, 0]);
    let mut fb = alloc::vec![0u32; 4 * 2];
    // SAFETY: as above, stride 4.
    unsafe { present(&back, Rect::new(0, 0, 1, 1), fb.as_mut_ptr(), 4, 1) };
    assert_eq!(fb[0], 0x332211);
}

#[test]
fn with_the_renders_the_ground_is_the_wallpaper_and_the_strip_its_art() {
    // A wallpaper the screen's size, a gradient; a strip 8 x 4 with 2 px
    // corners, its middle 0x445566 and its corners 0x112233.
    let wall: Vec<u32> = (0..400 * 300).map(|i| 0xFF00_0000 | (i % 400) << 8 | (i / 400)).collect();
    let mut sp = Vec::new();
    for y in 0..4u32 {
        for x in 0..8u32 {
            let corner = !(2..6).contains(&x) && !(1..3).contains(&y);
            let [r, g, b] = if corner { [0x11, 0x22, 0x33] } else { [0x44, 0x55, 0x66] };
            sp.extend_from_slice(&[b, g, r, 255]);
        }
    }
    let strip_bytes = fimage::fimg::encode(8, 4, 4, [2, 1, 2, 1], &sp);
    let strip = fimage::fimg::parse(&strip_bytes).unwrap();
    let art = ThemeArt { ground: wall.clone(), strip, strip_focused: strip };
    let mut ws = Windows::new(400, 300, 32);
    ws.create("Terminal", Kind::Normal, (200, 100), (0, 0), (0, 0), ME);
    let mut back = Back::new(400, 300);
    let mut f = fonts();
    let mut look = Look { theme: Theme::new(Mode::Dark, 1.0), fonts: &mut f, art: Some(&art) };
    let all = back.rect();
    compose(&mut back, all, &ws, &|_| None, &mut look);
    assert_eq!(at(&back, 5, 5), wall[5 * 400 + 5] & 0xFF_FFFF, "the wallpaper's pixel, not the token");
    assert_eq!(at(&back, 390, 290), wall[290 * 400 + 390] & 0xFF_FFFF);
    // The frame at (100, 84), 200 wide, its strip 32 tall: the strip's
    // render 9-sliced to it, not the token.
    let token = Theme::new(Mode::Dark, 1.0).colour(Tok::Surface2).xrgb();
    let want = strip_pixels(&strip, 200, 32, token);
    for (x, y) in [(250, 100), (230, 88), (240, 112)] {
        let got = at(&back, x, y);
        assert_eq!(got, want[((y - 84) * 200 + (x - 100)) as usize] & 0xFF_FFFF, "the strip at ({x}, {y})");
        assert_ne!(got, token);
    }
    // A damaged corner of the ground alone: still the wallpaper there.
    compose(&mut back, Rect::new(10, 10, 4, 4), &ws, &|_| None, &mut look);
    assert_eq!(at(&back, 12, 12), wall[12 * 400 + 12] & 0xFF_FFFF);
}
