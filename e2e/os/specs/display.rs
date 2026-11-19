// SPDX-License-Identifier: Apache-2.0
//! The native display (TASK-071): on a 16-inch 2560×1600 panel (the OMEN's;
//! in the VM, the stand-in EDID a dev or e2e bootloader reads when the
//! firmware gives none), the bootloader sets the panel's own mode and stage
//! starts at the scale that size suits.

use crate::Ctx;
use std::time::{Duration, Instant};

/// `@display.native`: the firmware started at its own mode; the bootloader
/// switched to the panel's 2560×1600; stage says 2560×1600 at 150 %, and the
/// screen it shows is that size.
pub fn native(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("using the stand-in EDID.BIN")?;
    ctx.expect_log("freally-boot: display 2560x1600")?;
    ctx.expect_log("stage: 2560x1600, starting at 150 %")?;
    // Asked from the first moment stage can be reached, it answers at once
    // (TASK-118's soak: stage stamped S3 after registering, so the door's
    // call and that stamp each waited on the other for 2 s, and the
    // desktop's first call timed out with them).
    let start = Instant::now();
    let said = loop {
        let said = ctx.door("stage list")?;
        if said.first().is_some_and(|l| l == "error: stage isn't up") && start.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        break said;
    };
    if !said.iter().any(|l| l == "scale 150") {
        return Err(format!("stage doesn't say 150 %: {said:?}"));
    }
    ctx.expect_log("desktop: the Rack is up")?;
    ctx.expect_no_log("desktop: the screen:")?;
    let shot = ctx.screenshot()?;
    if (shot.w, shot.h) != (2560, 1600) {
        return Err(format!("the screen is {}×{}, not 2560×1600", shot.w, shot.h));
    }
    ctx.keep_shot("display-native")?;
    Ok(())
}
