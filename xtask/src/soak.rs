// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask soak [--hours N]` (TASK-118): the CDP OS leg, round after
//! round, for N hours (24): every spec but the ones whose setups crash, cut
//! power or tamper on purpose. Each round's VMs boot fresh, on ports of
//! their own (so the CDP leg can run beside it). A round's result
//! and any kernel panic its VMs logged go to `target/soak/soak.log` as it
//! goes; at the end, the summary: rounds, specs run, every failure by spec,
//! panics. Green only with zero failures and zero panics.
//!
//! A file `target/soak/pause` holds it between rounds (another session's
//! heavy build needs the memory): the round going on finishes, the soak
//! says it's paused, and the time paused doesn't count toward the N hours.

use crate::{cdp, util};
use std::io::Write;
use std::time::{Duration, Instant};

/// Setups that crash, reset, fall back or tamper on purpose: their panics
/// are the point, so they stay out of the soak.
const PLANTED: [&str; 9] = [
    "e2e-crash-ud2",
    "e2e-ab",
    "e2e-ab-good",
    "e2e-gpt-ab",
    "e2e-tampered",
    "release-with-dev-image",
    "e2e-gpt-update",
    "e2e-gpt-power",
    "e2e-gpt-carry",
];

/// A round's red: each spec that failed, and `LEG` when the leg ended badly
/// with no spec to blame (a build that broke, a VM that never came up).
fn red(stdout: &str, ok: bool) -> Vec<String> {
    let mut fails: Vec<String> = stdout
        .lines()
        .filter_map(|l| l.strip_prefix("▶ @").filter(|l| l.contains("… fail")))
        .map(|l| l.split(' ').next().unwrap_or("").to_string())
        .collect();
    if !ok && fails.is_empty() {
        fails.push(String::from(LEG));
    }
    fails
}

/// What a broken leg is counted as.
const LEG: &str = "the leg";

fn line(log: &mut std::fs::File, text: &str) {
    println!("{text}");
    let _ = writeln!(log, "{text}");
}

/// The panics in the soak's own VMs' COM1 logs after a round (`setups`: theirs).
/// What the e2e test door's planted kernel panic says (the spec that proves
/// a panic reaches Monitor on the next boot asks for it).
const PLANTED_PANIC: &str = "a test panic, asked for through the e2e test door";

/// The kernel panics in one VM's log that nobody asked for.
fn unplanned_panics(log: &str) -> usize {
    log.lines()
        .filter(|l| (l.contains("freally-core: ERROR") || l.contains("PANIC")) && !l.contains(PLANTED_PANIC))
        .count()
}

fn panics(setups: &[String]) -> usize {
    let vms = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".freally-vms");
    let mut n = 0;
    for e in std::fs::read_dir(vms).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.strip_prefix("soak-").is_some_and(|s| setups.iter().any(|x| x == s)) {
            continue;
        }
        let text = String::from_utf8_lossy(&std::fs::read(e.path().join("com1.log")).unwrap_or_default()).to_string();
        n += unplanned_panics(&text);
    }
    n
}

pub fn soak_cmd(args: &[String]) -> Result<(), String> {
    let hours: f64 = util::flag(args, "--hours")
        .map(|h| h.parse().map_err(|_| "--hours takes a number"))
        .transpose()?
        .unwrap_or(24.0);
    let kept: Vec<(String, String)> =
        cdp::features()?.into_iter().filter(|(_, setup)| !PLANTED.contains(&setup.as_str())).collect();
    let ids: Vec<String> = kept.iter().map(|(id, _)| id.clone()).collect();
    let mut setups: Vec<String> = kept.into_iter().map(|(_, s)| s).collect();
    setups.sort();
    setups.dedup();
    let dir = util::target_dir().join("soak");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut log =
        std::fs::OpenOptions::new().create(true).append(true).open(dir.join("soak.log")).map_err(|e| e.to_string())?;
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let start = Instant::now();
    let mut paused = Duration::ZERO;
    let mut legs_broken = 0;
    let until = Duration::from_secs_f64(hours * 3600.0);
    line(&mut log, &format!("soak: {} specs a round, for {hours} h", ids.len()));
    let (mut rounds, mut run, mut failed, mut panicked) = (0usize, 0usize, Vec::<String>::new(), 0usize);
    while start.elapsed() - paused < until {
        if dir.join("pause").exists() {
            line(&mut log, &format!("soak: paused after round {rounds} (target/soak/pause)"));
            let t = Instant::now();
            while dir.join("pause").exists() {
                std::thread::sleep(Duration::from_secs(10));
            }
            paused += t.elapsed();
            line(&mut log, &format!("soak: going on after {:.0} min paused", t.elapsed().as_secs_f64() / 60.0));
        }
        rounds += 1;
        let t = Instant::now();
        let out = std::process::Command::new(&me)
            .args(["cdp", "--os", &format!("--only={}", ids.join(","))])
            .env(cdp::SOAK, "1")
            .output()
            .map_err(|e| format!("the CDP leg: {e}"))?;
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let errs = String::from_utf8_lossy(&out.stderr).to_string();
        let _ = std::fs::write(
            dir.join(format!("round-{rounds}.txt")),
            format!(
                "{text}
--- stderr
{errs}"
            ),
        );
        let fails = red(&text, out.status.success());
        // Why a leg broke, in the log: its last words.
        let broke = fails.iter().any(|f| f == LEG);
        if broke {
            let why = errs.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("it stopped");
            line(&mut log, &format!("round {rounds}: the leg broke: {}", why.trim()));
        }
        legs_broken = if broke { legs_broken + 1 } else { 0 };
        let passed = text.lines().filter(|l| l.starts_with("▶ @") && l.contains("… pass")).count();
        let p = panics(&setups);
        run += passed + fails.len();
        panicked += p;
        line(
            &mut log,
            &format!(
                "round {rounds}: {passed} of {} green in {:.1} min; panics {p}{}",
                passed + fails.len(),
                t.elapsed().as_secs_f64() / 60.0,
                if fails.is_empty() { String::new() } else { format!("; red: {}", fails.join(", ")) }
            ),
        );
        failed.extend(fails);
        // A build that's broken breaks every round: stop rather than spin.
        if legs_broken == 3 {
            line(&mut log, "soak: the leg broke three rounds in a row: stopped");
            break;
        }
    }
    let mut by = std::collections::BTreeMap::<&str, usize>::new();
    for f in &failed {
        *by.entry(f).or_default() += 1;
    }
    line(
        &mut log,
        &format!(
            "soak: {rounds} rounds in {:.1} h, {run} specs run, {} failed, {panicked} kernel panics{}",
            (start.elapsed() - paused).as_secs_f64() / 3600.0,
            failed.len(),
            if by.is_empty() {
                String::new()
            } else {
                format!(" ({})", by.iter().map(|(s, n)| format!("{s} ×{n}")).collect::<Vec<_>>().join(", "))
            }
        ),
    );
    if failed.is_empty() && panicked == 0 { Ok(()) } else { Err("the soak saw failures or panics".into()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_planted_test_panic_is_not_counted() {
        let planted = format!(
            "[monitor] up
freally-core: ERROR Freally Core panicked: {PLANTED_PANIC}. At kernel/src/syscall.rs:567.
"
        );
        assert_eq!(unplanned_panics(&planted), 0, "asked for by a spec");
        assert_eq!(
            unplanned_panics(
                "freally-core: ERROR Freally Core panicked: index out of bounds
"
            ),
            1
        );
        assert_eq!(
            unplanned_panics(
                "PANIC: a double fault
"
            ),
            1
        );
    }

    #[test]
    fn a_leg_that_breaks_with_no_failed_spec_is_red() {
        let one = "▶ @boot.title … pass (0.1s)
";
        assert_eq!(red(one, false), [LEG], "the build broke after the first setup");
        assert!(red(one, true).is_empty());
        let failed = "▶ @a.b … fail (1.0s)
    why
▶ @c.d … pass (1.0s)
";
        assert_eq!(red(failed, false), ["a.b"], "a failed spec says it");
    }
}
