// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask ci [--fast|--slow] [--only=a,b]`: every gate, reported one by one
//! as `▶ name … pass (12.3s)` (the fleet's format, which `ci-gates.mjs` reads),
//! then `ci:local: N of M gates green (T min)`.
//!
//! `--fast` skips clippy; `--slow` adds the long fuzz runs. A lock file stops
//! `ci-linux` starting beside it (16 GB of RAM is shared with the fleet).

use crate::{atlas, build, gates, util};
use fcore_abi::Flavour;
use std::process::Command;
use std::time::Instant;

pub const HOST_FEATURES: &str = "fsi/build,ffont/raster";

type Gate = (&'static str, Box<dyn Fn() -> Result<String, String>>);

/// Runs a command, capturing its output; on failure the last lines are the error.
fn quiet(mut c: Command) -> Result<String, String> {
    let out = c.output().map_err(|e| format!("couldn't start {:?}: {e}", c.get_program()))?;
    if out.status.success() {
        return Ok(String::new());
    }
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    Err(lines[lines.len().saturating_sub(40)..].join("\n"))
}

fn cargo(args: &[&str]) -> Command {
    let mut c = util::cargo();
    c.args(args);
    c
}

pub fn gates(fast: bool, slow: bool, container: bool) -> Vec<Gate> {
    let mut g: Vec<Gate> = Vec::new();
    g.push((
        "fmt",
        Box::new(|| quiet(cargo(&["fmt", "--all", "--", "--check"])).map(|_| "every file formatted".into())),
    ));
    if !fast {
        g.push((
            "clippy host",
            Box::new(|| {
                quiet(cargo(&[
                    "clippy",
                    "--workspace",
                    "--exclude",
                    "freally-boot",
                    "--exclude",
                    "freally-core",
                    "--all-targets",
                    "--features",
                    HOST_FEATURES,
                    "--",
                    "-D",
                    "warnings",
                ]))
                .map(|_| "no warnings".into())
            }),
        ));
        for f in ["release", "dev", "e2e"] {
            let name: &'static str = match f {
                "release" => "clippy boot/release",
                "dev" => "clippy boot/dev",
                _ => "clippy boot/e2e",
            };
            g.push((
                name,
                Box::new(move || {
                    let feat = format!("flavour-{f}");
                    quiet(cargo(&[
                        "clippy",
                        "-p",
                        "freally-boot",
                        "--target",
                        "x86_64-unknown-uefi",
                        "--features",
                        &feat,
                        "--",
                        "-D",
                        "warnings",
                    ]))
                    .map(|_| "no warnings".into())
                }),
            ));
        }
    }
    if !fast {
        for f in ["release", "dev", "e2e", "ktest"] {
            let name: &'static str = match f {
                "release" => "clippy kernel/release",
                "dev" => "clippy kernel/dev",
                "ktest" => "clippy kernel/ktest",
                _ => "clippy kernel/e2e",
            };
            g.push((
                name,
                Box::new(move || {
                    let feat =
                        if f == "ktest" { "flavour-dev,ktest,crash-ud2".to_string() } else { format!("flavour-{f}") };
                    quiet(cargo(&[
                        "clippy",
                        "-p",
                        "freally-core",
                        "--target",
                        "x86_64-unknown-none",
                        "--features",
                        &feat,
                        "--",
                        "-D",
                        "warnings",
                    ]))
                    .map(|_| "no warnings".into())
                }),
            ));
        }
    }
    if !fast {
        g.push((
            "clippy user",
            Box::new(|| {
                // User space on its own target: the runtime's `target_os = "none"`
                // code (`_start`, the heap, the panic path) only exists here.
                let mut c = cargo(&["clippy", "--release"]);
                c.args(build::user_target_args());
                for p in build::PROGRAMS.iter().chain(build::TEST_PROGRAMS) {
                    c.args(["--package", p.package]);
                }
                // e2e compiles in the most (conductor's test door).
                let f: Vec<String> = build::PROGRAMS
                    .iter()
                    .chain(build::TEST_PROGRAMS)
                    .map(|p| format!("{}/flavour-e2e", p.package))
                    .collect();
                c.args(["--features", &f.join(",")]);
                c.args(["--", "-D", "warnings"]);
                quiet(c).map(|_| "no warnings".into())
            }),
        ));
    }
    g.push((
        "host tests",
        Box::new(move || {
            let mut c = cargo(&[
                "test",
                "--workspace",
                "--exclude",
                "freally-boot",
                "--exclude",
                "freally-core",
                "--features",
                HOST_FEATURES,
            ]);
            if slow {
                c.env("FREALLY_FUZZ_ITERS", "2000");
            }
            quiet(c).map(|_| if slow { "green, fuzz at 2,000 iterations".into() } else { "green".into() })
        }),
    ));
    g.push((
        "ktests",
        Box::new(|| {
            let (results, verdict) = crate::ktest::run(600)?;
            let failed: Vec<String> =
                results.iter().filter(|r| !r.pass).map(|r| format!("{}: {}", r.name, r.why.join(" ").trim())).collect();
            match verdict {
                Ok(s) => Ok(s.trim_start_matches("ktests: ").to_string()),
                Err(e) if failed.is_empty() => Err(e),
                Err(e) => Err(format!(
                    "{e}
{}",
                    failed.join(
                        "
"
                    )
                )),
            }
        }),
    ));
    g.push((
        "flavours",
        Box::new(move || {
            for f in [Flavour::Release, Flavour::Dev, Flavour::E2e] {
                build::build_with(f, !container)?;
            }
            Ok(if container {
                "all three built and packed (no keys in the container)".into()
            } else {
                "all three built, packed and signed".into()
            })
        }),
    ));
    g.push(("no-third-party", Box::new(gates::third_party::check)));
    g.push(("no-AI", Box::new(gates::content::no_ai)));
    g.push(("denylist", Box::new(gates::content::denylist)));
    g.push(("SPDX", Box::new(gates::content::spdx)));
    g.push(("private-docs", Box::new(gates::content::private_docs)));
    g.push((
        "boot atlas",
        Box::new(|| {
            let fresh = atlas::render()?;
            let committed = std::fs::read(util::repo_root().join(atlas::ATLAS_PATH)).map_err(|e| e.to_string())?;
            if fresh == committed {
                Ok("what the Noto fonts give".into())
            } else {
                Err("stale or edited: cargo xtask atlas".into())
            }
        }),
    ));
    g.push(("ladder", Box::new(gates::ladder::check)));
    g.push(("site", Box::new(crate::site::check)));
    g
}

pub fn ci_cmd(args: &[String]) -> Result<(), String> {
    let fast = args.iter().any(|a| a == "--fast");
    let slow = args.iter().any(|a| a == "--slow");
    let container = args.iter().any(|a| a == "--container");
    let only: Option<Vec<String>> = args
        .iter()
        .find_map(|a| a.strip_prefix("--only="))
        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect());
    let lock = util::target_dir().join("ci.lock");
    std::fs::create_dir_all(util::target_dir()).map_err(|e| e.to_string())?;
    std::fs::write(&lock, std::process::id().to_string()).map_err(|e| e.to_string())?;
    let start = Instant::now();
    let all = gates(fast, slow, container);
    let selected: Vec<&Gate> =
        all.iter().filter(|(n, _)| only.as_ref().is_none_or(|o| o.iter().any(|x| x == n))).collect();
    let (mut green, mut skipped, mut red) = (0, 0, Vec::new());
    for (name, f) in &selected {
        let t = Instant::now();
        let r = f();
        let secs = t.elapsed().as_secs_f64();
        match r {
            Ok(note) if note.starts_with("SKIP") => {
                skipped += 1;
                println!("▶ {name} … skip ({secs:.1}s)\n    {note}");
            }
            Ok(note) => {
                green += 1;
                println!("▶ {name} … pass ({secs:.1}s)\n    {note}");
            }
            Err(e) => {
                red.push(*name);
                println!("▶ {name} … fail ({secs:.1}s)");
                for l in e.lines() {
                    println!("    {l}");
                }
            }
        }
    }
    let _ = std::fs::remove_file(&lock);
    let mins = start.elapsed().as_secs_f64() / 60.0;
    let total = selected.len() - skipped;
    if red.is_empty() {
        println!("ci:local: {green} of {total} gates green, {skipped} skipped ({mins:.1} min)");
        Ok(())
    } else {
        println!("ci:local: {green} of {total} gates green; red: {} ({mins:.1} min)", red.join(", "));
        Err(format!("{} gate(s) red", red.len()))
    }
}
