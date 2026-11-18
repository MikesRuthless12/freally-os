// SPDX-License-Identifier: Apache-2.0
//! fui: the Freally UI toolkit (Phase 3).
//!
//! - `tokens_gen`: the Freally UI kit's tokens, per theme, generated from
//!   `assets/kit/theme.css` (`cargo xtask sync-kit`; never edited).
//! - `tokens_os`: the desktop's own measures, which the kit doesn't have.
#![no_std]

pub mod color;
pub mod tokens_gen;
pub mod tokens_os;
