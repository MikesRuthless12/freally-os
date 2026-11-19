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
    Ok(())
}
