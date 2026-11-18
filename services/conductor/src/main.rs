// SPDX-License-Identifier: Apache-2.0
//! conductor, PID 1 (TASK-030, v0): says it started, then keeps the fault
//! reports the kernel delivers (TASK-031). Manifests, services and restarts
//! with backoff arrive in Phase 2 (TASK-038).
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

mod faults;

fstd::program!(run);

fn run(_bootstrap: u32) -> i64 {
    fstd::info!("conductor: started");
    let mut kept = faults::Kept::new();
    loop {
        if let Err(e) = kept.wait_and_take() {
            fstd::error!("conductor: can't read fault reports: {}", e.describe());
            fstd::time::sleep(fstd::time::Duration::from_secs(1));
        }
    }
}
