// SPDX-License-Identifier: Apache-2.0
//! The runtime theme (TASK-055): dark or light (the UI kit's two, no skins),
//! the UI scale, and the type scale (vision § 5). Switching either is a
//! repaint, never a rebuild: nodes name tokens, the theme resolves them.

use crate::color::Rgba;
use crate::tokens_gen::{dark, light};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Dark,
    Light,
}

/// The colours a node may name; the theme gives each its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tok {
    Bg,
    Surface,
    Surface2,
    Border,
    Primary,
    PrimaryInk,
    OnPrimary,
    Focus,
    Text,
    Text2,
    Text3,
    Success,
    Warning,
    Error,
    MeterGreen,
    MeterYellow,
    MeterRed,
    MeterOff,
}

/// The type scale (vision § 5): size and line height at 100 %, and weight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextStyle {
    Caption,
    Small,
    Body,
    Subtitle,
    Title,
    Display,
    Clock,
    Mono,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weight {
    Regular,
    Medium,
    SemiBold,
}

impl TextStyle {
    /// (size px, line height px, weight) at 100 %.
    pub const fn spec(self) -> (u32, u32, Weight) {
        match self {
            TextStyle::Caption => (12, 16, Weight::Regular),
            TextStyle::Small => (13, 18, Weight::Regular),
            TextStyle::Body => (14, 20, Weight::Regular),
            TextStyle::Subtitle => (16, 22, Weight::Medium),
            TextStyle::Title => (20, 26, Weight::SemiBold),
            TextStyle::Display => (28, 34, Weight::SemiBold),
            TextStyle::Clock => (32, 36, Weight::Medium),
            TextStyle::Mono => (14, 20, Weight::Regular),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub mode: Mode,
    /// 1.0 is 100 %; the OS starts at the display's density (≈1.5 on the OMEN).
    pub scale: f32,
}

impl Theme {
    pub const fn new(mode: Mode, scale: f32) -> Theme {
        Theme { mode, scale }
    }

    pub fn colour(&self, t: Tok) -> Rgba {
        let d = self.mode == Mode::Dark;
        macro_rules! pick {
            ($name:ident) => {
                if d { dark::$name } else { light::$name }
            };
        }
        match t {
            Tok::Bg => pick!(COLOR_BG),
            Tok::Surface => pick!(COLOR_SURFACE),
            Tok::Surface2 => pick!(COLOR_SURFACE_2),
            Tok::Border => pick!(COLOR_BORDER),
            Tok::Primary => pick!(COLOR_PRIMARY),
            Tok::PrimaryInk => pick!(COLOR_PRIMARY_INK),
            Tok::OnPrimary => pick!(COLOR_ON_PRIMARY),
            Tok::Focus => pick!(COLOR_FOCUS),
            Tok::Text => pick!(COLOR_TEXT),
            Tok::Text2 => pick!(COLOR_TEXT_2),
            Tok::Text3 => pick!(COLOR_TEXT_3),
            Tok::Success => pick!(COLOR_SUCCESS),
            Tok::Warning => pick!(COLOR_WARNING),
            Tok::Error => pick!(COLOR_ERROR),
            Tok::MeterGreen => pick!(COLOR_METER_GREEN),
            Tok::MeterYellow => pick!(COLOR_METER_YELLOW),
            Tok::MeterRed => pick!(COLOR_METER_RED),
            Tok::MeterOff => pick!(COLOR_METER_OFF),
        }
    }

    /// A length at this scale, in whole pixels.
    pub fn px(&self, v: u32) -> u32 {
        (v as f32 * self.scale + 0.5) as u32
    }

    /// A text style's size and line height at this scale.
    pub fn text(&self, s: TextStyle) -> (f32, u32, Weight) {
        let (size, line, w) = s.spec();
        (size as f32 * self.scale, self.px(line), w)
    }
}
