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

/// The test kernel's summary line (`ktests: 384 of 384 pass`), not its
/// first (`ktests: 48 to run, on each of 8 CPUs`).
fn is_summary(l: &str) -> bool {
    l.starts_with("ktests: ") && l.contains(" of ") && l.trim_end().ends_with(" pass")
}

/// Whether the test kernel has said its summary on COM1.
fn summary_said(o: &VmOptions) -> bool {
    std::fs::read_to_string(o.com1_log()).is_ok_and(|l| l.lines().any(is_summary))
}

/// Builds, boots and reads the test kernel. `Ok((results, verdict))`; `Err` only
/// when it couldn't run at all.
pub fn run(timeout_secs: u64) -> Result<(Vec<Outcome>, Result<String, String>), String> {
    let timeout = Duration::from_secs(timeout_secs);
    let built = build::build_full(Flavour::Dev, true, &["ktest"], &[])?;
    let mut o = VmOptions::new(Flavour::Dev, "ktest");
    o.headless = true;
    // Every test on each of 8 CPUs (TASK-036).
    o.cpus = 8;
    o.tcg = !std::path::Path::new("/dev/kvm").exists();
    o.port_offset = 500;
    let start = Instant::now();
    let mut vm = run::start_built(&o, &built)?;
    let mut summary_at: Option<Instant> = None;
    let mut wakes = 0u64;
    let code = loop {
        if let Some(status) = vm.try_wait().map_err(|e| e.to_string())? {
            break status.code();
        }
        // QEMU 10.2's exit after the kernel's isa-debug-exit write can wait for
        // its main loop to wake: after the ktests' runtime SetVariable calls,
        // 2 of 5 runs were still up 60 s after the summary, each gone 0.3 s
        // after a QMP command (none of 6 without those calls). So once the
        // kernel has said its summary, a QMP command every 2 s until QEMU
        // goes; the verdict is still the kernel's exit code.
        if summary_at.is_none() && summary_said(&o) {
            summary_at = Some(Instant::now());
        }
        if let Some(t) = summary_at
            && t.elapsed() > Duration::from_secs(2 * (wakes + 1))
        {
            wakes += 1;
            let said = cdp_proto::qmp::Qmp::connect(&o.qmp_socket(), Duration::from_secs(2))
                .and_then(|mut q| q.execute("query-status", None).map(|_| ()));
            eprintln!("ktest: QEMU still up {} s after the kernel's summary: woke its main loop ({said:?})", 2 * wakes);
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
    let summary = log.lines().find(|l| is_summary(l));
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

#[cfg(test)]
mod tests {
    #[test]
    fn the_summary_is_the_last_line_not_the_first() {
        assert!(super::is_summary("ktests: 384 of 384 pass"));
        assert!(super::is_summary("ktests: 383 of 384 pass "));
        assert!(!super::is_summary("ktests: 48 to run, on each of 8 CPUs"));
        assert!(!super::is_summary("ktest time::x … pass (0 Mticks)"));
    }
}
