// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask boot-check [--boots=N]` (TASK-065): N (5) KVM boots, each read
//! through its test door once the desktop takes input and has settled.
//!
//! - **The gate:** the median B0 → S4 is at most 3,000 ms (the target is 1,000).
//! - **Sub-budgets,** printed per stage (over is a warning, not red): B0→B3
//!   ≤ 250 ms, K0→C0 ≤ 150 ms, C0→S3 ≤ 450 ms, S3→S4 ≤ 150 ms.
//! - **Idle:** at most 12 processes, each with a description; at most 192 MiB
//!   in use.
//!
//! The release flavour has no door to read it by, so these are the e2e image's
//! boots: an upper bound on release's (e2e also starts cdpd and logs to COM1).

use crate::build;
use crate::cdp::{HOST_PORT, tool};
use crate::run::{self, VmOptions};
use crate::util;
use fcore_abi::Flavour;
use fjson::Value;
use std::process::{Command, Stdio};

const GATE_MS: f64 = 3000.0;
const TARGET_MS: f64 = 1000.0;
const MAX_PROCESSES: usize = 12;
const MAX_MEMORY: f64 = 192.0 * 1024.0 * 1024.0;
/// Each sub-budget: from, to, ms.
const BUDGETS: [(&str, &str, f64); 4] =
    [("B0", "B3", 250.0), ("K0", "C0", 150.0), ("C0", "S3", 450.0), ("S3", "S4", 150.0)];
const PORT_OFFSET: u16 = 700;

struct Boot {
    stages: Vec<(String, f64)>,
    processes: Vec<(String, String)>,
    mem: f64,
}

impl Boot {
    fn at(&self, stage: &str) -> Option<f64> {
        self.stages.iter().find(|(s, _)| s == stage).map(|(_, ms)| *ms)
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    if v.is_empty() { 0.0 } else { v[v.len() / 2] }
}

fn one_boot(built: &build::Built, cpus: u32) -> Result<Boot, String> {
    let mut o = VmOptions::new(Flavour::E2e, "boot-check");
    o.cpus = cpus;
    o.headless = true;
    o.tcg = !std::path::Path::new("/dev/kvm").exists();
    o.port_offset = PORT_OFFSET;
    let port = HOST_PORT + PORT_OFFSET;
    let mut vm = run::start_built(&o, built)?;
    let com3 = format!("127.0.0.1:{}", o.port(3));
    let host = Command::new(tool("cdp-host"))
        .args(["--qmp", &o.qmp_socket().display().to_string(), "--port", &port.to_string(), "--com3", &com3])
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| format!("can't start cdp-host: {e}"));
    let out = host.as_ref().map_err(Clone::clone).and_then(|_| {
        Command::new(tool("cdp-runner"))
            .args(["--boot-check", "--endpoint", &format!("127.0.0.1:{port}")])
            .args(["--com1-log", &o.com1_log().display().to_string()])
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| format!("can't start cdp-runner: {e}"))
    });
    // FREALLY_KEEP_VM=1: a boot that can't be read is left running (its
    // monitor socket, `hmp.sock`, shows where each CPU is).
    let failed = out.as_ref().map_or(true, |o| !o.status.success());
    if failed && std::env::var("FREALLY_KEEP_VM").is_ok_and(|v| v == "1") {
        return Err(format!("the boot couldn't be read; its VM is left running in {}", o.dir().display()));
    }
    if let Ok(mut h) = host {
        let _ = h.kill();
        let _ = h.wait();
    }
    let _ = vm.kill();
    let _ = vm.wait();
    let out = out?;
    if !out.status.success() {
        return Err(String::from("the boot couldn't be read through its door"));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let v = fjson::parse(text.lines().last().unwrap_or("")).map_err(|e| format!("the runner's line: {e:?}"))?;
    let stages = v
        .get("stages")
        .and_then(Value::as_array)
        .unwrap_or_default()
        .iter()
        .filter_map(|s| Some((s.get("name")?.as_str()?.to_string(), s.get("ms")?.as_f64()?)))
        .collect();
    let processes = v
        .get("processes")
        .and_then(Value::as_array)
        .unwrap_or_default()
        .iter()
        .filter(|p| p.get("state").and_then(Value::as_str) == Some("Running"))
        .filter_map(|p| Some((p.get("name")?.as_str()?.to_string(), p.get("description")?.as_str()?.to_string())))
        .collect();
    Ok(Boot { stages, processes, mem: v.get("memUsed").and_then(Value::as_f64).unwrap_or(0.0) })
}

pub fn boot_check_cmd(args: &[String]) -> Result<(), String> {
    let num = |k: &str, d: usize| args.iter().find_map(|a| a.strip_prefix(k)).and_then(|s| s.parse().ok()).unwrap_or(d);
    check(num("--boots=", 5), num("--cpus=", 2) as u32).map(|summary| println!("boot-check: green: {summary}"))
}

/// The boots (on `cpus` CPUs) and their verdict: a summary line, or what's over.
pub fn check(boots: usize, cpus: u32) -> Result<String, String> {
    util::run_ok(util::cargo().args(["build", "--quiet", "-p", "cdp-host", "-p", "cdp-runner"]))?;
    let built = build::build_full(Flavour::E2e, true, &[], &[])?;
    let mut all = Vec::new();
    for i in 1..=boots {
        let b = one_boot(&built, cpus)?;
        println!(
            "boot {i}: B0→S4 {:.0} ms, {} processes, {:.0} MiB in use",
            b.at("S4").unwrap_or(f64::NAN),
            b.processes.len(),
            b.mem / 1048576.0
        );
        all.push(b);
    }
    let s4 = median(all.iter().filter_map(|b| b.at("S4")).collect());
    println!("\nmedian of {boots}, each stage after B0:");
    for name in ["B0", "B1", "B2", "B3", "K0", "K1", "K2", "K3", "C0", "S1", "S2", "S3", "S4"] {
        let ms: Vec<f64> = all.iter().filter_map(|b| b.at(name)).collect();
        if !ms.is_empty() {
            println!("  {name}  {:>7.1} ms", median(ms));
        }
    }
    println!("sub-budgets (median):");
    let mut warnings = 0;
    for (from, to, budget) in BUDGETS {
        let d = median(all.iter().filter_map(|b| Some(b.at(to)? - b.at(from)?)).collect());
        let over = d > budget;
        warnings += over as usize;
        println!("  {from}→{to}  {d:>7.1} ms of {budget:.0}{}", if over { "  ⚠ over (a warning)" } else { "" });
    }
    let procs = all.iter().map(|b| b.processes.len()).max().unwrap_or(0);
    let undescribed: Vec<String> = all
        .iter()
        .flat_map(|b| b.processes.iter())
        .filter(|(_, d)| d.trim().is_empty())
        .map(|(n, _)| n.clone())
        .collect();
    let mem = all.iter().map(|b| b.mem).fold(0.0, f64::max);
    if let Some(b) = all.first() {
        println!("idle processes ({}):", b.processes.len());
        for (n, d) in &b.processes {
            println!("  {n:<14} {d}");
        }
    }
    println!(
        "\nB0→S4 median {s4:.0} ms (gate {GATE_MS:.0}, target {TARGET_MS:.0}); at most {procs} processes (≤ {MAX_PROCESSES}); at most {:.0} MiB in use (≤ 192)",
        mem / 1048576.0
    );
    let mut red = Vec::new();
    if all.iter().any(|b| b.at("S4").is_none()) {
        red.push(String::from("a boot never reached S4"));
    }
    if s4 > GATE_MS {
        red.push(format!("the median boot is {s4:.0} ms, over {GATE_MS:.0}"));
    }
    if procs > MAX_PROCESSES {
        red.push(format!("{procs} processes at idle, over {MAX_PROCESSES}"));
    }
    if !undescribed.is_empty() {
        red.push(format!("processes with no description: {}", undescribed.join(", ")));
    }
    if mem > MAX_MEMORY {
        red.push(format!("{:.0} MiB in use at idle, over 192", mem / 1048576.0));
    }
    if red.is_empty() {
        Ok(format!(
            "B0→S4 median {s4:.0} ms of {boots} boots (gate {GATE_MS:.0}), {procs} processes, {:.0} MiB at idle{}",
            mem / 1048576.0,
            if warnings > 0 { format!(", {warnings} sub-budget warning(s)") } else { String::new() }
        ))
    } else {
        Err(format!("boot-check: {}", red.join("; ")))
    }
}
