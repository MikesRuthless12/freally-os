// SPDX-License-Identifier: Apache-2.0
//! The CI gates. Each returns `Ok(summary)` or `Err(what's wrong)`.

pub mod content;
pub mod ladder;
pub mod third_party;
