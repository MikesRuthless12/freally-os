// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask cdp --os [--site] [--only=id,id] [--slow]`: the CDP legs
//! (closeout step 5a; `--slow` runs the long forms, such as `@serial.echo`'s 10 MB).
//!
//! The OS leg: for each VM setup in `e2e/os/coverage.json`, boot it headless,
//! start `cdp-host` on its QMP socket, and run that setup's specs with
//! `cdp-runner`. Each spec prints `▶ @id … pass (1.2s)`; the end prints how many
//! are green and the leg's time. Every coverage feature must have a spec and
//! every spec a coverage feature.
//!
//! The site leg (TASK-033): `cdp-runner --site site/` serves the built docs
//! site, starts headless Chrome and runs `e2e/site/coverage.json`'s specs over
//! CDP. Where the site or Chrome isn't here it says SKIP with why.

use crate::build;
use crate::run::{self, VmOptions};
use crate::util;
use fcore_abi::Flavour;
use fjson::Value;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::Instant;

const HOST_PORT: u16 = 9433;
const PORT_OFFSET: u16 = 200;

struct Feature {
    id: String,
    setup: String,
}

fn coverage() -> Result<(Value, Vec<Feature>), String> {
    let path = util::repo_root().join("e2e/os/coverage.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v = fjson::parse(&text).map_err(|e| format!("coverage.json at byte {}: {}", e.at, e.what))?;
    let mut features = Vec::new();
    for f in v.get("features").and_then(Value::as_array).ok_or("coverage.json has no features")? {
        let id = f.get("id").and_then(Value::as_str).ok_or("a feature without an id")?;
        for key in ["task", "setup", "spec", "what"] {
            if f.get(key).and_then(Value::as_str).is_none_or(str::is_empty) {
                return Err(format!("feature {id} has no {key}"));
            }
        }
        let setup = f.get("setup").and_then(Value::as_str).unwrap_or_default();
        if v.get("setups").and_then(|s| s.get(setup)).is_none() {
            return Err(format!("feature {id} names an unknown setup {setup:?}"));
        }
        features.push(Feature { id: id.into(), setup: setup.into() });
    }
    Ok((v, features))
}

fn tool(name: &str) -> std::path::PathBuf {
    util::target_dir().join("debug").join(name)
}

/// `--slow`: the specs' long forms (the runner reads `FREALLY_CDP_SLOW`).
static SLOW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn cdp_cmd(args: &[String]) -> Result<(), String> {
    SLOW.store(args.iter().any(|a| a == "--slow"), std::sync::atomic::Ordering::Relaxed);
    let site = args.iter().any(|a| a == "--site");
    let os = args.iter().any(|a| a == "--os") || !site;
    let only: Option<Vec<String>> =
        args.iter().find_map(|a| a.strip_prefix("--only=")).map(|s| s.split(',').map(String::from).collect());
    let start = Instant::now();
    let (mut green, mut total) = (0usize, 0usize);
    if os {
        let (g, t) = os_leg(only.as_deref())?;
        green += g;
        total += t;
    }
    if site {
        let (g, t) = site_leg(only.as_deref())?;
        green += g;
        total += t;
    }
    let secs = start.elapsed().as_secs_f64();
    if green == total {
        println!("All {total} CDP specs pass ({:.1} min)", secs / 60.0);
        Ok(())
    } else {
        println!("{} gate(s) red: {green} of {total} CDP specs green ({:.1} min)", total - green, secs / 60.0);
        Err("the CDP leg is red".into())
    }
}

fn os_leg(only: Option<&[String]>) -> Result<(usize, usize), String> {
    let (cov, features) = coverage()?;
    util::run_ok(util::cargo().args(["build", "--quiet", "-p", "cdp-host", "-p", "cdp-runner"]))?;
    // Coverage and the runner's specs name the same features.
    let listed = util::capture(Command::new(tool("cdp-runner")).arg("--list"))?;
    let specs: Vec<&str> = listed.lines().collect();
    for f in &features {
        if !specs.contains(&f.id.as_str()) {
            return Err(format!("coverage feature {} has no spec in cdp-runner", f.id));
        }
    }
    for s in &specs {
        if !features.iter().any(|f| f.id == *s) {
            return Err(format!("spec {s} isn't in e2e/os/coverage.json"));
        }
    }

    let mut setups: Vec<&str> = Vec::new();
    for f in &features {
        if !setups.contains(&f.setup.as_str()) {
            setups.push(&f.setup);
        }
    }
    let (mut green, mut total) = (0, 0);
    for setup in setups {
        let ids: Vec<&str> = features
            .iter()
            .filter(|f| f.setup == setup && only.is_none_or(|o| o.iter().any(|x| x == &f.id)))
            .map(|f| f.id.as_str())
            .collect();
        if ids.is_empty() {
            continue;
        }
        let s = cov.get("setups").and_then(|s| s.get(setup)).ok_or("no setup")?;
        let flavour = crate::pack::parse_flavour(s.get("flavour").and_then(Value::as_str).unwrap_or("e2e"))?;
        let mut o = VmOptions::new(flavour, &format!("cdp-{setup}"));
        o.headless = true;
        o.tcg = !std::path::Path::new("/dev/kvm").exists();
        o.port_offset = PORT_OFFSET;
        o.tamper_byte = s.get("tamper").and_then(Value::as_i64).map(|b| b as usize);
        o.no_ps2 = s.get("no_ps2").and_then(Value::as_bool).unwrap_or(false);
        // Slot B: a crashing image, or this build's own (kept apart: the next build reuses the folder).
        o.slot_b = match s.get("slot_b").and_then(Value::as_str) {
            Some("crash-test") => Some(build::crash_test(flavour)?),
            Some("same") => {
                let b = build::build_full(flavour, true, &[], &[])?;
                let out = b.dir.join("SYSTEM_B.FSI");
                std::fs::copy(&b.image, &out).map_err(|e| e.to_string())?;
                Some(out)
            }
            Some(other) => return Err(format!("setup {setup}: slot_b is crash-test or same, not {other:?}")),
            None => None,
        };
        if let Some(from) = s.get("image_from").and_then(Value::as_str) {
            o.image_override = Some(build::build(crate::pack::parse_flavour(from)?)?.image);
        }
        let features: Vec<String> = s
            .get("kernel_features")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
            .unwrap_or_default();
        let features: Vec<&str> = features.iter().map(String::as_str).collect();
        let without: Vec<String> = s
            .get("without")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
            .unwrap_or_default();
        let without: Vec<&str> = without.iter().map(String::as_str).collect();
        let built = build::build_full(flavour, true, &features, &without)?;
        let (g, t) = run_setup(setup, &o, &built, &ids)?;
        green += g;
        total += t;
    }
    let _ = Flavour::E2e;
    Ok((green, total))
}

fn run_setup(setup: &str, o: &VmOptions, built: &build::Built, ids: &[&str]) -> Result<(usize, usize), String> {
    let mut vm = run::start_built(o, built)?;
    let mut host = Command::new(tool("cdp-host"))
        .args(["--qmp", &o.qmp_socket().display().to_string(), "--port", &HOST_PORT.to_string()])
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| format!("can't start cdp-host: {e}"))?;
    let failures = util::target_dir().join("cdp/failures");
    let slow = SLOW.load(std::sync::atomic::Ordering::Relaxed);
    let runner = Command::new(tool("cdp-runner"))
        .env("FREALLY_CDP_SLOW", if slow { "1" } else { "0" })
        .args([
            "--endpoint",
            &format!("127.0.0.1:{HOST_PORT}"),
            "--com1-log",
            &o.com1_log().display().to_string(),
            "--specs",
            &ids.join(","),
            "--failures",
            &failures.display().to_string(),
        ])
        .args(if o.flavour == Flavour::E2e {
            vec![String::from("--com3"), format!("127.0.0.1:{}", o.port(3))]
        } else {
            Vec::new()
        })
        .stdout(Stdio::piped())
        .spawn();
    let mut result = Ok((0, ids.len()));
    match runner {
        Ok(mut r) => {
            let mut green = 0;
            if let Some(out) = r.stdout.take() {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    if line.starts_with("▶ ") && line.contains(" … pass (") {
                        green += 1;
                    }
                    println!("{line}");
                }
            }
            let _ = r.wait();
            result = Ok((green, ids.len()));
        }
        Err(e) => result = result.and(Err(format!("can't start cdp-runner: {e}"))),
    }
    let _ = host.kill();
    let _ = host.wait();
    let _ = vm.kill();
    let _ = vm.wait();
    let _ = setup;
    result
}

/// The docs-site leg: every feature in `e2e/site/coverage.json`, one spec each.
fn site_leg(only: Option<&[String]>) -> Result<(usize, usize), String> {
    let root = util::repo_root();
    let site = root.join("site");
    if !site.join("site.json").is_file() {
        println!("▶ site leg … skip (0.0s)\n    the docs site (its own repository) isn't checked out in site/");
        return Ok((0, 0));
    }
    if Command::new(crate::cdp_chrome()).arg("--version").output().is_err() {
        println!("▶ site leg … skip (0.0s)\n    no headless Chrome here ({}; set FREALLY_CHROME)", crate::cdp_chrome());
        return Ok((0, 0));
    }
    crate::site::check()?;
    let path = root.join("e2e/site/coverage.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v = fjson::parse(&text).map_err(|e| format!("site coverage.json at byte {}: {}", e.at, e.what))?;
    let mut ids = Vec::new();
    for f in v.get("features").and_then(Value::as_array).ok_or("site coverage.json has no features")? {
        let id = f.get("id").and_then(Value::as_str).ok_or("a site feature without an id")?;
        for key in ["task", "spec", "what"] {
            if f.get(key).and_then(Value::as_str).is_none_or(str::is_empty) {
                return Err(format!("site feature {id} has no {key}"));
            }
        }
        ids.push(id.to_string());
    }
    util::run_ok(util::cargo().args(["build", "--quiet", "-p", "cdp-runner"]))?;
    let listed = util::capture(Command::new(tool("cdp-runner")).arg("--list-site"))?;
    let listed: Vec<&str> = listed.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    for id in &ids {
        if !listed.contains(&id.as_str()) {
            return Err(format!("site feature {id} has no spec in cdp-runner"));
        }
    }
    for s in &listed {
        if !ids.iter().any(|i| i == s) {
            return Err(format!("site spec {s} isn't in e2e/site/coverage.json"));
        }
    }
    let run: Vec<&str> =
        ids.iter().map(String::as_str).filter(|i| only.is_none_or(|o| o.iter().any(|x| x == i))).collect();
    if run.is_empty() {
        return Ok((0, 0));
    }
    let mut r = Command::new(tool("cdp-runner"))
        .args(["--site", &site.display().to_string(), "--specs", &run.join(",")])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("can't start cdp-runner: {e}"))?;
    let mut green = 0;
    if let Some(out) = r.stdout.take() {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if line.starts_with("▶ ") && line.contains(" … pass (") {
                green += 1;
            }
            println!("{line}");
        }
    }
    let _ = r.wait();
    Ok((green, run.len()))
}
