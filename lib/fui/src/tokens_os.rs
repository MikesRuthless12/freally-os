// SPDX-License-Identifier: Apache-2.0
//! Tokens the OS needs and the UI kit (made for apps in a window) doesn't
//! have: the desktop's own measures, at 100% scale. Sizes grow with the UI
//! scale; colours always come from the kit (`tokens_gen`).

/// The window title strip's height.
pub const TITLE_STRIP_H: u32 = 32;
/// The Rack (the desktop's bar) height.
pub const RACK_H: u32 = 56;
/// A window's corner radius.
pub const WINDOW_RADIUS: u32 = 10;
/// The gap between a window's edge and its content.
pub const WINDOW_PAD: u32 = 12;
/// The spacing scale: every gap is one of these.
pub const SPACE: [u32; 6] = [4, 8, 12, 16, 24, 32];
