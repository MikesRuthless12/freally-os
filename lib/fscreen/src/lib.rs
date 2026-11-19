// SPDX-License-Identifier: Apache-2.0
//! The screens drawn before the desktop exists, in Noto Sans from the boot atlas
//! (`assets/fonts/noto/boot-atlas.fatl`, made by `cargo xtask atlas`).
//!
//! The bootloader draws them, and the CDP runner draws the same screen on the
//! host to know the exact pixels to expect, so a spec can compare at tolerance 0.
//! No allocation: the caller owns the pixels (`0x00RRGGBB`, row by row).
#![no_std]

use ffont::atlas::{self, Atlas, Strike, Target};

/// The boot atlas: strike 0 is Noto Sans SemiBold at 44 px (titles), strike 1
/// Noto Sans Regular at 18 px (text).
pub const ATLAS: &[u8] = include_bytes!("../../../assets/fonts/noto/boot-atlas.fatl");

/// The characters the boot atlas holds (`cargo xtask atlas` renders exactly these).
pub const ATLAS_CHARS: &str =
    " !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~·–—…‘’“”";
pub const TITLE_PX: f32 = 44.0;
pub const TEXT_PX: f32 = 18.0;

/// The dark theme's colours (product-vision § 5; the UI kit's tokens win from Phase 3).
/// The boot screens wear the Freally UI kit's dark theme (they come before
/// any setting could choose light): its ground, green, words and error red.
pub mod colours {
    use fui::tokens_gen::dark as kit;

    pub const BACKGROUND: u32 = kit::COLOR_BG.xrgb();
    pub const PRIMARY: u32 = kit::COLOR_PRIMARY.xrgb();
    pub const TEXT: u32 = kit::COLOR_TEXT.xrgb();
    pub const TEXT_MUTED: u32 = kit::COLOR_TEXT_2.xrgb();
    pub const ERROR: u32 = kit::COLOR_ERROR.xrgb();
}

/// Pixels the caller owns.
pub struct Canvas<'a> {
    pub px: &'a mut [u32],
    pub w: usize,
    pub h: usize,
}

impl Target for Canvas<'_> {
    fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }
    fn get(&self, x: usize, y: usize) -> u32 {
        self.px[y * self.w + x]
    }
    fn set(&mut self, x: usize, y: usize, rgb: u32) {
        self.px[y * self.w + x] = rgb;
    }
    fn fill(&mut self, rgb: u32) {
        self.px.fill(rgb);
    }
}

/// A rectangle in pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

fn strikes() -> Option<(Strike<'static>, Strike<'static>)> {
    let a = Atlas::parse(ATLAS)?;
    Some((a.strike(0)?, a.strike(1)?))
}

/// Draws one centred line; returns its box (the strike's ascent to descent).
fn centred(c: &mut impl Target, s: &Strike, baseline: i32, parts: &[&str], rgb: u32) -> Rect {
    let width = s.measure_parts(parts);
    let x = (c.size().0 as i32 - width) / 2;
    atlas::draw_parts(c, s, x, baseline, parts, rgb);
    Rect { x, y: baseline - s.ascent as i32, w: width, h: (s.ascent + s.descent) as i32 }
}

/// The boot title: *Freally OS vX.Y.Z* in Freally green, and a quiet line under it.
/// Returns the title's box (what `@boot.title` compares).
pub fn title(c: &mut impl Target, version: &str, note: &str) -> Rect {
    c.fill(colours::BACKGROUND);
    let Some((big, small)) = strikes() else {
        return Rect { x: 0, y: 0, w: 0, h: 0 };
    };
    let baseline = c.size().1 as i32 / 2;
    let r = centred(c, &big, baseline, &["Freally OS v", version], colours::PRIMARY);
    centred(c, &small, baseline + 2 * small.line_height as i32, &[note], colours::TEXT_MUTED);
    r
}

/// The integrity screen (wording: the owner, 2026-11-17).
pub fn integrity(c: &mut impl Target, reason: &str) -> Rect {
    c.fill(colours::BACKGROUND);
    let Some((big, small)) = strikes() else {
        return Rect { x: 0, y: 0, w: 0, h: 0 };
    };
    let baseline = c.size().1 as i32 / 2 - big.line_height as i32 / 2;
    let r = centred(c, &big, baseline, &["Freally OS didn’t start"], colours::ERROR);
    let line = small.line_height as i32;
    centred(c, &small, baseline + 2 * line, &[reason], colours::TEXT);
    centred(
        c,
        &small,
        baseline + 3 * line,
        &["Nothing from this image was run. Copy a fresh image to this drive to start again."],
        colours::TEXT_MUTED,
    );
    r
}

/// The panic screen (FR-005): what stopped, where, and that nothing was lost
/// that hadn't been saved. `lines` are drawn under the title, one per row.
pub fn panic(c: &mut impl Target, lines: &[&str]) -> Rect {
    c.fill(colours::BACKGROUND);
    let Some((big, small)) = strikes() else {
        return Rect { x: 0, y: 0, w: 0, h: 0 };
    };
    let line = small.line_height as i32;
    let baseline = c.size().1 as i32 / 2 - (lines.len() as i32 * line) / 2;
    let r = centred(c, &big, baseline, &["Freally OS stopped"], colours::ERROR);
    for (i, l) in lines.iter().enumerate() {
        let colour = if i == 0 { colours::TEXT } else { colours::TEXT_MUTED };
        centred(c, &small, baseline + (2 + i as i32) * line, &[l], colour);
    }
    r
}

/// The log on the screen (TASK-074's `log=screen`, dev only): a heading, then
/// the newest lines that fit, oldest at the top, left-aligned; errors in the
/// error red, warnings in the plain text colour, the rest muted. Returns how
/// many lines were drawn.
pub fn log(c: &mut impl Target, lines: &[(&str, u8)]) -> usize {
    c.fill(colours::BACKGROUND);
    let Some((_, small)) = strikes() else { return 0 };
    let line = small.line_height as i32;
    let (w, h) = c.size();
    let margin = 16;
    let mut baseline = margin + small.ascent as i32;
    atlas::draw_parts(
        c,
        &small,
        margin,
        baseline,
        &["Freally OS · the log (log=screen), until the desktop starts; then Monitor’s Log tab has it"],
        colours::PRIMARY,
    );
    baseline += line + line / 2;
    let room = ((h as i32 - baseline - margin) / line).max(0) as usize + 1;
    let shown = &lines[lines.len().saturating_sub(room)..];
    for (text, level) in shown {
        let colour = match level {
            4.. => colours::ERROR,
            3 => colours::TEXT,
            _ => colours::TEXT_MUTED,
        };
        // A line wider than the screen is cut at a character boundary.
        let mut end = text.len();
        while end > 0 && small.measure(&text[..end]) > w as i32 - 2 * margin {
            end -= 1;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
        }
        atlas::draw_parts(c, &small, margin, baseline, &[&text[..end]], colour);
        baseline += line;
    }
    shown.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_keeps_the_newest_lines_that_fit() {
        let (w, h) = (640, 200);
        let mut px = [0u32; 640 * 200];
        let mut c = Canvas { px: &mut px, w, h };
        let owned: [(&str, u8); 40] = core::array::from_fn(|i| (if i % 2 == 0 { "devmgr: a line" } else { "x" }, 2));
        let mut lines = owned;
        lines[39] = ("stage: the last line, an error", 4);
        let n = log(&mut c, &lines);
        assert!(n > 2 && n < 40, "only what fits: {n}");
        // Something drawn in the error red (the newest line, at the bottom).
        assert!(c.px.contains(&colours::ERROR) || c.px.iter().any(|&p| p != colours::BACKGROUND));
        // Nothing to show: the heading alone.
        assert_eq!(log(&mut c, &[]), 0);
    }

    #[test]
    fn a_line_too_wide_is_cut_not_overflowed() {
        let mut px = [0u32; 200 * 80];
        let mut c = Canvas { px: &mut px, w: 200, h: 80 };
        let long = "é".repeat(300);
        assert_eq!(log(&mut c, &[(long.as_str(), 2)]), 1);
    }
}
