// SPDX-License-Identifier: Apache-2.0
//! The boot specs (`@boot.*`): the bootloader's screens, compared with what
//! `fscreen` draws on the host for the same screen size, pixel for pixel.

use crate::{Ctx, Expect};

/// `@boot.title`: verified, and the title painted.
pub fn title(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_screen(Expect::Title { note: "Freally Core is running." })?;
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
    ctx.expect_screen(Expect::Title { note: "Freally Core is running." })?;
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

/// `@boot.handoff`: boot services exit, the runtime services move to the direct
/// map, and Freally Core is entered with a BootInfo it accepts (TASK-018).
/// `@boot.conductor`: Freally Core starts conductor from the image as PID 1
/// (its ELF re-checked against the TOC, loaded W^X); conductor says it started,
/// and the kernel stamps C0 when it first enters user mode (TASK-030).
pub fn conductor(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("freally-core: scheduler ready (SMEP")?;
    ctx.expect_log("freally-core: conductor started (C0 ")?;
    ctx.expect_log("[pid 1] conductor: started")?;
    let line = ctx.log_line_with("conductor started (C0 ").ok_or("no C0 line")?;
    let ms: u64 = line
        .split("(C0 ")
        .nth(1)
        .and_then(|s| s.split(' ').next())
        .and_then(|n| n.parse().ok())
        .ok_or("the C0 line has no time")?;
    if ms == 0 || ms > 3_000 {
        return Err(format!("C0 came {ms} ms after B0 (the boot budget is 3,000 ms)"));
    }
    ctx.expect_screen(Expect::Title { note: "Freally Core is running." })
}

pub fn handoff(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("freally-boot: boot services exited")?;
    ctx.expect_log("freally-boot: runtime services moved")?;
    ctx.expect_log("freally-boot: jumping to Freally Core")?;
    ctx.expect_log("freally-core 0.1.0 entered")?;
    ctx.expect_log("freally-core: memory ready")?;
    ctx.expect_screen(Expect::Title { note: "Freally Core is running." })
}
