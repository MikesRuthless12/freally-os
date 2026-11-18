// SPDX-License-Identifier: Apache-2.0
//! The boot specs (`@boot.*`): the bootloader's screens, compared with what
//! `fscreen` draws on the host for the same screen size, pixel for pixel.

use crate::{Ctx, Expect};

/// `@boot.title`: verified, and the title painted.
pub fn title(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_screen(Expect::Title { note: "Every block checked. Signed by the dev key." })?;
    ctx.expect_log("freally-boot: verified Freally OS")?;
    ctx.expect_log("signed by the dev key")
}

/// `@boot.integrity`: a flipped bit in block 2 is refused, with the reason shown.
pub fn integrity(ctx: &mut Ctx) -> Result<(), String> {
    ctx.allow_errors(&["image refused"]);
    ctx.expect_screen(Expect::Integrity { reason: fsi::FsiError::BadBlock(2).describe() })?;
    ctx.expect_log("BadBlock(2)")?;
    ctx.expect_no_log("showing the title")
}

/// `@boot.release`: the release bootloader boots its staging-signed image.
pub fn release(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_screen(Expect::Title { note: "Every block checked. Signed by the staging key." })?;
    ctx.expect_log("freally-boot 0.1.0 (release)")?;
    ctx.expect_log("signed by the staging key")
}

/// `@boot.seal`: a release bootloader refuses a dev-signed image.
pub fn seal(ctx: &mut Ctx) -> Result<(), String> {
    ctx.allow_errors(&["image refused"]);
    ctx.expect_screen(Expect::Integrity { reason: fsi::FsiError::UnknownKey.describe() })?;
    ctx.expect_log("(UnknownKey)")?;
    ctx.expect_no_log("showing the title")
}
