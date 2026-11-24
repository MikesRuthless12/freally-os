// SPDX-License-Identifier: Apache-2.0
//! The kernel specs (`@kernel.*`).

use crate::{Ctx, Expect};

/// `@kernel.panic_screen`: a kernel fault (a deliberate `ud2` in a test build)
/// stops on the panic screen naming the exception and its RIP; the screen is
/// drawn exactly as the kernel's own log line describes it (TASK-019).
/// `@kernel.last_panic`: a kernel panic writes the last-panic page; after a
/// warm reboot (QMP `system_reset`, RAM kept) the next boot files it as a
/// fault report from `freally-core` and logs it (TASK-031).
pub fn last_panic(ctx: &mut Ctx) -> Result<(), String> {
    ctx.allow_errors(&["An invalid instruction (#UD) in Freally Core"]);
    ctx.expect_log("freally-core: ERROR An invalid instruction (#UD) in Freally Core at 0xffffffff8")?;
    ctx.expect_no_log("freally-core: the last boot stopped:")?;
    ctx.reset()?;
    ctx.expect_log("freally-core: the last boot stopped: An invalid instruction (#UD) in Freally Core at 0xffffffff8")?;
    ctx.expect_log("freally-core: fault in freally-core (pid 0)")?;
    // The test build stops again on the same ud2: one report per boot, never two.
    ctx.expect_log_count("freally-core: ERROR An invalid instruction (#UD)", 2)?;
    if ctx.log_count("the last boot stopped:") != 1 {
        return Err("the last-panic page was filed more than once".into());
    }
    Ok(())
}

/// `@kernel.timer_oneshot`: on a CPU without TSC-deadline (as VirtualBox shows
/// a guest), Freally Core's timer runs in the APIC's one-shot mode, its clock
/// measured against the TSC, and the boot reaches the desktop: every sleep
/// and every wait with a deadline wakes (TASK-113).
pub fn timer_oneshot(ctx: &mut Ctx) -> Result<(), String> {
    ctx.expect_log("freally-core: no TSC-deadline: the APIC timer counts one-shot at ")?;
    ctx.expect_log("desktop: the Rack is up")?;
    ctx.expect_log(" S4=")
}

pub fn panic_screen(ctx: &mut Ctx) -> Result<(), String> {
    ctx.allow_errors(&["An invalid instruction (#UD) in Freally Core"]);
    ctx.expect_log("freally-core: ERROR An invalid instruction (#UD) in Freally Core at 0xffffffff8")?;
    let line = ctx.log_line_with("freally-core: ERROR ").ok_or("no ERROR line")?;
    let body = line.split_once("ERROR ").map(|x| x.1).unwrap_or("");
    let (what, detail) = body.split_once(". Error code").ok_or("the ERROR line has no detail")?;
    let lines = [
        format!("{what}."),
        format!("Error code{detail}"),
        "Freally OS stopped so nothing else could go wrong. Restart the computer to carry on.".to_string(),
    ];
    ctx.expect_screen(Expect::Panic { lines })
}
