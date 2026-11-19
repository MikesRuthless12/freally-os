// SPDX-License-Identifier: Apache-2.0
//! Storage on the real driver (TASK-092): drv-nvme on QEMU's NVMe
//! controller, the disk's GPT read, the partition interlock refusing a write
//! to block 0 before the controller sees it, and a block of FREALLY_DATA
//! written, flushed and read back.

use crate::Ctx;

/// `@nvme.interlock`.
pub fn interlock(ctx: &mut Ctx) -> Result<(), String> {
    for line in [
        "drv-nvme: QEMU NVMe Ctrl",
        "drv-nvme: partition FREALLY_SYSTEM_A",
        "drv-nvme: partition FREALLY_DATA",
        "drv-nvme: e2e: a write to block 0 (the protective MBR) refused: AccessDenied",
        "of FREALLY_DATA written, flushed and read back the same",
    ] {
        ctx.expect_log(line)?;
    }
    if ctx.log_count("drv-nvme: partition EFI system partition") != 1 {
        return Err("drv-nvme didn't list the ESP once".into());
    }
    // MSI-X (TASK-092A): completions interrupt, and the driver is woken by them.
    ctx.expect_log("drv-nvme: MSI-X on: completions interrupt on vector")?;
    let woken = ctx.log_lines_with("command(s) so far woken by their interrupt");
    let n: u32 = woken
        .last()
        .and_then(|l| l.split("drv-nvme: ").nth(1))
        .and_then(|l| l.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    if n == 0 {
        return Err(format!("no command was woken by its interrupt: {woken:?}"));
    }
    Ok(())
}
