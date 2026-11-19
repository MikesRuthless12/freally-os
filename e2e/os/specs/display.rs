// SPDX-License-Identifier: Apache-2.0
//! The native display (TASK-071): on a 16-inch 2560×1600 panel (the OMEN's;
//! in the VM, the stand-in EDID a dev or e2e bootloader reads when the
//! firmware gives none), the bootloader sets the panel's own mode and stage
//! starts at the scale that size suits.

use crate::Ctx;

/// `@display.native`: the firmware started at its own mode; the bootloader
/// switched to the panel's 2560×1600; stage says 2560×1600 at 150 %, and the
/// screen it shows is that size.
pub fn native(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("using the stand-in EDID.BIN")?;
    ctx.expect_log("freally-boot: display 2560x1600")?;
    ctx.expect_log("stage: 2560x1600, starting at 150 %")?;
    if !ctx.door("stage list")?.iter().any(|l| l == "scale 150") {
        return Err(String::from("stage doesn't say 150 %"));
    }
    let shot = ctx.screenshot()?;
    if (shot.w, shot.h) != (2560, 1600) {
        return Err(format!("the screen is {}×{}, not 2560×1600", shot.w, shot.h));
    }
    ctx.keep_shot("display-native")?;
    Ok(())
}
