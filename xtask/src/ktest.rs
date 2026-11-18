// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask test`: the kernel's own tests (TASK-022). Builds the dev flavour
//! with the `ktest` feature, boots it headless (KVM, else TCG), and reads COM1:
//! one `ktest <module>::<name> … pass|fail` line per test, then the kernel exits
//! QEMU through `isa-debug-exit` (33 all pass, 35 one failed). A hang past the
//! timeout, a crash or a missing summary is a failure too.

use crate::build;
use crate::run::{self, VmOptions};
use fcore_abi::Flavour;
use std::time::{Duration, Instant};

/// One test's result: its name, whether it passed, and its reason lines.
pub struct Outcome {
    pub name: String,
    pub pass: bool,
    pub why: Vec<String>,
}

pub fn test_cmd(args: &[String]) -> Result<(), String> {
    let timeout = args.iter().find_map(|a| a.strip_prefix("--timeout=")).and_then(|s| s.parse().ok()).unwrap_or(180);
    let start = Instant::now();
    let (results, verdict) = run(timeout)?;
    for r in &results {
        println!(
            "▶ ktest {} … {} ({:.1}s)",
            r.name,
            if r.pass { "pass" } else { "fail" },
            start.elapsed().as_secs_f64()
        );
        for w in &r.why {
            println!("{w}");
        }
    }
    verdict.map(|summary| println!("{summary}"))
}

/// Builds, boots and reads the test kernel. `Ok((results, verdict))`; `Err` only
/// when it couldn't run at all.
pub fn run(timeout_secs: u64) -> Result<(Vec<Outcome>, Result<String, String>), String> {
    let timeout = Duration::from_secs(timeout_secs);
    let built = build::build_full(Flavour::Dev, true, &["ktest"], &[])?;
    let mut o = VmOptions::new(Flavour::Dev, "ktest");
    o.headless = true;
    o.tcg = !std::path::Path::new("/dev/kvm").exists();
    o.port_offset = 500;
    let start = Instant::now();
    let mut vm = run::start_built(&o, &built)?;
    let code = loop {
        if let Some(status) = vm.try_wait().map_err(|e| e.to_string())? {
            break status.code();
        }
        if start.elapsed() > timeout {
            let _ = vm.kill();
            let _ = vm.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let log = std::fs::read_to_string(o.com1_log()).unwrap_or_default().replace('\r', "");
    let mut results = Vec::new();
    let mut lines = log.lines().peekable();
    while let Some(l) = lines.next() {
        if let Some(rest) = l.strip_prefix("ktest ") {
            let name = rest.split(" … ").next().unwrap_or(rest).to_string();
            let pass = rest.contains(" … pass");
            let mut why = Vec::new();
            while let Some(next) = lines.peek() {
                if next.starts_with("    ") {
                    why.push(next.to_string());
                    lines.next();
                } else {
                    break;
                }
            }
            results.push(Outcome { name, pass, why });
        }
    }
    let pass = results.iter().filter(|r| r.pass).count();
    let fail = results.len() - pass;
    let summary = log.lines().find(|l| l.starts_with("ktests: ") && l.contains(" of "));
    let mins = start.elapsed().as_secs_f64() / 60.0;
    let verdict = match (code, summary) {
        (Some(33), Some(_)) if fail == 0 => Ok(format!("ktests: {pass} of {pass} pass ({mins:.1} min)")),
        (Some(35), _) | (Some(33), _) => Err(format!("{fail} ktest(s) failed, {pass} passed")),
        (None, _) => {
            Err(format!("the test kernel didn't finish within {} s ({pass} passed so far)", timeout.as_secs()))
        }
        (Some(c), _) => {
            let tail: Vec<&str> = log.lines().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect();
            Err(format!("QEMU exited with {c} before the tests finished; COM1 ends:\n{}", tail.join("\n")))
        }
    };
    Ok((results, verdict))
}
