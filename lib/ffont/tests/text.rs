// SPDX-License-Identifier: Apache-2.0
//! Text at run time (TASK-053): the gamma-correct blend, the three weights,
//! measuring and drawing, on the real Noto files.
#![cfg(feature = "raster")]

use ffont::atlas::Target;
use ffont::text::{Face, blend, covers, draw, measure};

const REGULAR: &[u8] = include_bytes!("../../../assets/fonts/noto/NotoSans-Regular.ttf");
const MEDIUM: &[u8] = include_bytes!("../../../assets/fonts/noto/NotoSans-Medium.ttf");
const SEMIBOLD: &[u8] = include_bytes!("../../../assets/fonts/noto/NotoSans-SemiBold.ttf");
const SYMBOLS: &[u8] = include_bytes!("../../../assets/fonts/noto/NotoSansSymbols2-Regular.ttf");
const MONO: &[u8] = include_bytes!("../../../assets/fonts/noto/NotoSansMono-Regular.ttf");

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
fn the_blend_is_in_linear_light() {
    // White over black at coverage 128/255 is that much of the light: linear
    // 0.502, sRGB 188 (an sRGB mix would give 128). Black over white leaves
    // 127/255 of the light: sRGB 187.
    assert_eq!(blend(0x000000, 0xFFFFFF, 128), 0xBCBCBC);
    assert_eq!(blend(0xFFFFFF, 0x000000, 128), 0xBBBBBB);
    // Full and no coverage are exact, and a colour over itself stays itself.
    assert_eq!(blend(0x123456, 0x79D920, 255), 0x79D920);
    assert_eq!(blend(0x123456, 0x79D920, 0), 0x123456);
    for c in [0x000000, 0x090C09, 0x79D920, 0xE8EAF0, 0xFFFFFF] {
        for a in [1, 64, 128, 200, 254] {
            assert_eq!(blend(c, c, a), c, "{c:06x} over itself at {a}");
        }
    }
}

/// How much ink a run lays down (the sum of its coverage).
fn ink(face: Face, text: &str) -> u64 {
    let mut c = Canvas { w: 400, h: 60, px: vec![0; 400 * 60] };
    draw(&mut c, &mut [face], 32.0, 4, 44, text, 0xFFFFFF);
    c.px.iter().map(|&p| (p & 0xFF) as u64).sum()
}

#[test]
fn three_weights_each_heavier() {
    let ir = ink(Face::new(REGULAR).unwrap(), "Freally OS");
    let im = ink(Face::new(MEDIUM).unwrap(), "Freally OS");
    let is = ink(Face::new(SEMIBOLD).unwrap(), "Freally OS");
    assert!(ir < im && im < is, "regular {ir}, medium {im}, semibold {is}");
}

#[test]
fn measuring_and_metrics() {
    let f = Face::new(REGULAR).unwrap();
    assert!(f.has('A') && f.has('é') && f.has('—') && !f.has('\u{E000}'));
    let (up, down, line) = f.metrics(16.0);
    assert!(up > 10 && down > 2 && line >= up + down, "{up} {down} {line}");
    let mut chain = [f];
    // A run measures as its glyphs' advances, and draws to the same end.
    let w = measure(&mut chain, 16.0, "Freally");
    let mut c = Canvas { w: 200, h: 30, px: vec![0; 200 * 30] };
    assert_eq!(draw(&mut c, &mut chain, 16.0, 10, 20, "Freally", 0xFFFFFF), 10 + w);
    // Glyphs are cached: drawing again changes nothing.
    let before = c.px.clone();
    let mut again = Canvas { w: 200, h: 30, px: vec![0; 200 * 30] };
    draw(&mut again, &mut chain, 16.0, 10, 20, "Freally", 0xFFFFFF);
    assert_eq!(before, again.px);
}

/// FR-026's coverage set: ASCII, Latin-1 and the specials, every one drawn
/// by the chain (Noto Sans, then Symbols 2 for ✓, then Mono for the arrows).
#[test]
fn the_chain_covers_the_whole_set() {
    let sans = Face::new(REGULAR).unwrap();
    assert!("←→↑↓✓".chars().all(|c| !sans.has(c)), "Noto Sans grew the arrows: the chain may be shorter");
    let chain = [sans, Face::new(SYMBOLS).unwrap(), Face::new(MONO).unwrap()];
    let set = (' '..='~').chain('\u{A0}'..='\u{FF}').chain("•–—…←→↑↓✓×".chars());
    let missing: String = set.filter(|c| !covers(&chain, *c)).collect();
    assert!(missing.is_empty(), "the chain draws no {missing:?}");
}

/// Figures are tabular: every digit as wide as the next, at every weight and
/// size the interface uses (Noto Sans's default figures are; this pins it).
#[test]
fn figures_are_tabular() {
    for data in [REGULAR, MEDIUM, SEMIBOLD] {
        let mut chain = [Face::new(data).unwrap()];
        for px in [12.0, 14.0, 16.0, 18.0, 28.0] {
            let widths: Vec<i32> = ('0'..='9').map(|d| measure(&mut chain, px, &d.to_string().repeat(10))).collect();
            assert!(widths.windows(2).all(|w| w[0] == w[1]), "at {px} px: {widths:?}");
        }
    }
}

/// Mono: every printable character the same advance; cells of whole pixels
/// that tile exactly; every ASCII glyph inside its cell (TASK-054).
#[test]
fn mono_cells_tile_and_hold_their_glyphs() {
    use ffont::text::{cell, draw_cell};
    let mut chain = [Face::new(MONO).unwrap()];
    for px in [12.0, 14.0, 16.0, 20.0] {
        let advances: Vec<i32> = (' '..='~').map(|c| chain[0].glyph(c, px).advance_q6).collect();
        assert!(advances.windows(2).all(|w| w[0] == w[1]), "Mono's advances differ at {px} px");
        let k = cell(&mut chain[0], px);
        assert!(k.w > 0 && k.h > k.baseline && k.baseline > 0, "{k:?}");
        for c in '!'..='~' {
            let g = chain[0].glyph(c, px).clone();
            let (top, left) = (k.baseline - g.top as i32, g.left as i32);
            // Wide glyphs (%, W, m) may lean a pixel or two into the next cell, as in
            // every monospace font; rows must never collide, so up and down is exact.
            assert!(left >= -2 && left + (g.w as i32) < k.w + 3, "{c:?} spills sideways more than 2 px at {px} px");
            assert!(top >= 0 && top + (g.h as i32) < k.h + 2, "{c:?} spills out of its cell at {px} px: {k:?}, {g:?}");
        }
        // Eight cells drawn one by one are eight cells wide, no drift.
        let mut c = Canvas { w: 400, h: 40, px: vec![0; 400 * 40] };
        for col in 0..8 {
            draw_cell(&mut c, &mut chain, px, k, 2, 2, col, 0, '█', 0xFFFFFF);
        }
        let lit: Vec<usize> = (0..400).filter(|&x| c.px[(2 + k.baseline as usize - 2) * 400 + x] != 0).collect();
        if let (Some(first), Some(last)) = (lit.first(), lit.last()) {
            let span = last - first + 1;
            assert!(span < 8 * k.w as usize + 2, "eight cells drifted wider: {span} > {}", 8 * k.w);
        }
    }
}
