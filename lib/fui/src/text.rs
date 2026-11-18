// SPDX-License-Identifier: Apache-2.0
//! The interface's fonts (TASK-055): Noto Sans in three weights and Noto
//! Sans Mono, each a fallback chain (Symbols 2 for ✓, Mono for the arrows),
//! measured and drawn at the theme's scale.

use crate::theme::{TextStyle, Theme, Weight};
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
}
