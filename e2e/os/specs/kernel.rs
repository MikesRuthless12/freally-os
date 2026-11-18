// SPDX-License-Identifier: Apache-2.0
//! The kernel specs (`@kernel.*`).

use crate::{Ctx, Expect};

/// `@kernel.panic_screen`: a kernel fault (a deliberate `ud2` in a test build)
/// stops on the panic screen naming the exception and its RIP; the screen is
/// drawn exactly as the kernel's own log line describes it (TASK-019).
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
