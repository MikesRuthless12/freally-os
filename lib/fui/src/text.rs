// SPDX-License-Identifier: Apache-2.0
//! The interface's fonts (TASK-055): Noto Sans in three weights and Noto
//! Sans Mono, each a fallback chain (Symbols 2 for ✓, Mono for the arrows),
//! measured and drawn at the theme's scale.

use crate::theme::{TextStyle, Theme, Weight};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use ffont::text::{self, Face};
use ffont::ttf::FontError;

/// The font files, as the image carries them.
#[derive(Clone, Copy, Debug)]
pub struct FontData<'a> {
    pub regular: &'a [u8],
    pub medium: &'a [u8],
    pub semibold: &'a [u8],
    pub mono: &'a [u8],
    pub symbols: &'a [u8],
}

pub struct Fonts<'a> {
    chains: [Vec<Face<'a>>; 4],
}

impl core::fmt::Debug for Fonts<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Fonts")
    }
}

impl<'a> Fonts<'a> {
    pub fn new(d: FontData<'a>) -> Result<Fonts<'a>, FontError> {
        let chain = |main: &'a [u8]| -> Result<Vec<Face<'a>>, FontError> {
            Ok(vec![Face::new(main)?, Face::new(d.symbols)?, Face::new(d.mono)?])
        };
        Ok(Fonts {
            chains: [
                chain(d.regular)?,
                chain(d.medium)?,
                chain(d.semibold)?,
                vec![Face::new(d.mono)?, Face::new(d.symbols)?],
            ],
        })
    }

    /// The chain a text style draws with.
    pub fn chain(&mut self, style: TextStyle) -> &mut [Face<'a>] {
        let i = match (style, style.spec().2) {
            (TextStyle::Mono, _) => 3,
            (_, Weight::Regular) => 0,
            (_, Weight::Medium) => 1,
            (_, Weight::SemiBold) => 2,
        };
        &mut self.chains[i]
    }

    /// The width of `words` and the style's line height, in device pixels.
    pub fn measure(&mut self, theme: Theme, style: TextStyle, words: &str) -> (u32, u32) {
        let (px, line, _) = theme.text(style);
        (text::measure(self.chain(style), px, words).max(0) as u32, line)
    }

    /// `words` as they fit in `max` device pixels: whole when they do, else
    /// cut at a word where one ends late enough (a letter where none does)
    /// and ended with an ellipsis.
    pub fn fit(&mut self, theme: Theme, style: TextStyle, words: &str, max: u32) -> String {
        if self.measure(theme, style, words).0 <= max {
            return String::from(words);
        }
        let ends: Vec<usize> = words.char_indices().map(|(i, _)| i).skip(1).collect();
        let fits = |f: &mut Self, end: usize| {
            f.measure(theme, style, &alloc::format!("{}…", words[..end].trim_end())).0 <= max
        };
        // The longest cut that fits (a binary search: wider never fits better).
        let (mut lo, mut hi) = (0, ends.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if fits(self, ends[mid - 1]) { lo = mid } else { hi = mid - 1 }
        }
        let Some(&end) = lo.checked_sub(1).and_then(|i| ends.get(i)) else {
            return String::from("…");
        };
        // Back to the last space, unless that loses more than a third.
        let cut = match words[..end].rfind(' ') {
            Some(sp) if sp * 3 >= end * 2 => sp,
            _ => end,
        };
        alloc::format!("{}…", words[..cut].trim_end())
    }
}

/// Words on a surface in a text style and a token's colour, the top of their
/// line at `(x, y)`.
pub fn words(
    s: &mut fimage::blit::Surface,
    fonts: &mut Fonts,
    theme: Theme,
    style: TextStyle,
    (x, y): (i32, i32),
    text: &str,
    colour: crate::theme::Tok,
) {
    let (px, line, _) = theme.text(style);
    let chain = fonts.chain(style);
    let (up, down, _) = chain[0].metrics(px);
    let baseline = y + (line as i32 - (up + down)) / 2 + up;
    struct T<'a, 'b>(&'a mut fimage::blit::Surface<'b>);
    impl ffont::atlas::Target for T<'_, '_> {
        fn size(&self) -> (usize, usize) {
            (self.0.width as usize, self.0.height as usize)
        }
        fn get(&self, x: usize, y: usize) -> u32 {
            self.0.px[y * self.0.stride as usize + x]
        }
        fn set(&mut self, x: usize, y: usize, rgb: u32) {
            self.0.px[y * self.0.stride as usize + x] = rgb;
        }
    }
    text::draw(&mut T(s), chain, px, x, baseline, text, theme.colour(colour).xrgb());
}
