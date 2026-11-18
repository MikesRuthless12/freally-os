// SPDX-License-Identifier: Apache-2.0
//! The parser and rasterizer against the real Noto fonts in `assets/fonts/noto/`.
#![cfg(feature = "raster")]

use ffont::atlas::{self, Atlas, Target};
use ffont::ttf::{Font, render};

const REGULAR: &[u8] = include_bytes!("../../../assets/fonts/noto/NotoSans-Regular.ttf");
const MONO: &[u8] = include_bytes!("../../../assets/fonts/noto/NotoSansMono-Regular.ttf");

#[test]
fn noto_sans_parses_and_maps_characters() {
    let f = Font::parse(REGULAR).unwrap();
    assert_eq!(f.units_per_em, 1000);
    for c in ['A', 'g', '0', '?', 'é', 'ß', 'Ж', 'Ω'] {
        assert_ne!(f.glyph_index(c), 0, "{c:?} has no glyph");
    }
    // A code point no Latin font covers maps to the missing glyph.
    assert_eq!(f.glyph_index('\u{4E00}'), 0);
    assert!(f.ascender > 0 && f.descender < 0);
}

#[test]
fn mono_advances_are_all_equal() {
    let f = Font::parse(MONO).unwrap();
    let a = f.advance(f.glyph_index('i'));
    for c in "MWil0.".chars() {
        assert_eq!(f.advance(f.glyph_index(c)), a, "{c:?}");
    }
}

#[test]
fn glyphs_render_with_ink_where_expected() {
    let f = Font::parse(REGULAR).unwrap();
    let space = render(&f, f.glyph_index(' '), 32.0);
    assert_eq!((space.w, space.h), (0, 0));
    assert!(space.advance_q6 > 0);
    let l = render(&f, f.glyph_index('l'), 32.0);
    // "l" is a tall vertical stem: a fully covered pixel in its middle.
    assert!(l.alpha.contains(&255));
    let o = render(&f, f.glyph_index('o'), 64.0);
    // "o" has a hole: its centre pixel is empty, its ring isn't.
    let c = (o.h as usize / 2) * o.w as usize + o.w as usize / 2;
    assert_eq!(o.alpha[c], 0);
    assert!(o.alpha.iter().filter(|&&a| a == 255).count() > 50);
}

#[test]
fn composite_glyphs_render() {
    // Accented capitals are composites in Noto (a base glyph plus a mark).
    let f = Font::parse(REGULAR).unwrap();
    let e = render(&f, f.glyph_index('É'), 48.0);
    let plain = render(&f, f.glyph_index('E'), 48.0);
    assert!(e.h > plain.h, "the accent adds height");
    assert!(e.alpha.iter().filter(|&&a| a > 128).count() > plain.alpha.iter().filter(|&&a| a > 128).count());
}

#[test]
fn rendering_is_deterministic() {
    let f = Font::parse(REGULAR).unwrap();
    let a = atlas::build(&[(&f, 24.0, "Freally OS v0.1.0")]);
    let b = atlas::build(&[(&f, 24.0, "Freally OS v0.1.0")]);
    assert_eq!(a, b);
}

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

#[test]
fn an_atlas_round_trips_and_draws() {
    let f = Font::parse(REGULAR).unwrap();
    let bytes = atlas::build(&[(&f, 40.0, " ?FOSaelry"), (&f, 16.0, "abc")]);
    let a = Atlas::parse(&bytes).unwrap();
    let s = a.strike(0).unwrap();
    assert_eq!(s.px, 40);
    assert!(s.glyph('F').is_some());
    // Unknown characters fall back to "?".
    assert_eq!(s.glyph('Z').unwrap().w, s.glyph('?').unwrap().w);
    let mut c = Canvas { w: 300, h: 60, px: vec![0x0D0E10; 300 * 60] };
    let end = atlas::draw_text(&mut c, &s, 10, 45, "Freally OS", 0x79D920);
    assert_eq!(end - 10, s.measure("Freally OS"));
    assert!(c.px.contains(&0x79D920), "solid green pixels drawn");
    assert!(c.px.iter().any(|&p| p != 0x79D920 && p != 0x0D0E10), "anti-aliased edges");
    assert!(a.strike(2).is_none());
}

#[test]
fn a_damaged_font_is_refused_or_empty_never_a_crash() {
    assert!(Font::parse(&REGULAR[..100]).is_err());
    let mut bad = REGULAR.to_vec();
    // Scribble over the middle of the font (inside glyf).
    for b in &mut bad[200_000..200_400] {
        *b = 0xFF;
    }
    if let Ok(f) = Font::parse(&bad) {
        for c in 'A'..='z' {
            let _ = render(&f, f.glyph_index(c), 20.0);
        }
    }
}
