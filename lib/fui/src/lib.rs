// SPDX-License-Identifier: Apache-2.0
//! fui: the Freally UI toolkit (Phase 3).
//!
//! - `tokens_gen`: the Freally UI kit's tokens, per theme, generated from
//!   `assets/kit/theme.css` (`cargo xtask sync-kit`; never edited).
//! - `tokens_os`: the desktop's own measures, which the kit doesn't have.
//! - `theme`: dark or light, the UI scale, and the type scale.
//! - `tree`, `layout`, `paint`: the retained widget tree, laid out in two
//!   passes (row, column, grid, stack, scroll) and painted.
//! - `text`: Noto in three weights and Mono, as fallback chains.
//! - `strings`: the catalogs; no interface words in code.
//! - `a11y`: every node's role, name, state, value, bounds and test id.
//!
//! The tree and everything that allocates is feature `ui`; without it, fui
//! is the colours, tokens and theme the bootloader's screens use.
#![no_std]

#[cfg(feature = "ui")]
extern crate alloc;

#[cfg(feature = "ui")]
pub mod a11y;
pub mod color;
#[cfg(feature = "ui")]
pub mod layout;
#[cfg(feature = "ui")]
pub mod paint;
#[cfg(feature = "ui")]
pub mod strings;
#[cfg(feature = "ui")]
pub mod text;
pub mod theme;
pub mod tokens_gen;
pub mod tokens_os;
#[cfg(feature = "ui")]
pub mod tree;
