// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask bench ipc`: the channel round-trip benchmark (TASK-028). Boots a
//! kernel built with `bench-ipc`, which ping-pongs 10,000 calls between two
//! threads and prints the median; the target is ≤ 5 µs under KVM.

use crate::build;
use crate::run::{self, VmOptions};
use fcore_abi::Flavour;
use std::time::{Duration, Instant};

pub fn bench_cmd(args: &[String]) -> Result<(), String> {
    if args.first().map(String::as_str) != Some("ipc") {
        return Err("cargo xtask bench ipc".into());
    }
    let ns = ipc()?;
    println!("bench ipc: median {ns} ns per round trip (10,000 calls)");
    Ok(())
}

/// The median channel round trip (ns); an error above the 5 µs target.
pub fn ipc() -> Result<u64, String> {
    let built = build::build_full(Flavour::Dev, true, &["bench-ipc"])?;
    let mut o = VmOptions::new(Flavour::Dev, "bench");
    o.headless = true;
    o.port_offset = 700;
    let start = Instant::now();
    let mut vm = run::start_built(&o, &built)?;
    loop {
        if vm.try_wait().map_err(|e| e.to_string())?.is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(120) {
            let _ = vm.kill();
            let _ = vm.wait();
            return Err("the benchmark didn't finish in 120 s".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let log = std::fs::read_to_string(o.com1_log()).unwrap_or_default();
    let line = log.lines().find(|l| l.starts_with("bench ipc:")).ok_or("the kernel printed no result")?;
    let ns: u64 = line.split_whitespace().nth(3).and_then(|n| n.parse().ok()).ok_or("an unreadable result")?;
    if ns > 5_000 {
        return Err(format!("the median round trip is {ns} ns; the target is 5,000 ns"));
    }
    Ok(ns)
}
