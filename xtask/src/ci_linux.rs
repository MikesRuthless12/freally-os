// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask ci-linux`: the Linux leg (closeout step 5), after `ci`, never
//! beside it.
//!
//! 1. Refuses while `cargo xtask ci` runs (its lock file), and when Windows has
//!    under 6 GB free (the fleet's 4 GB floor plus room for the container).
//! 2. Builds the three flavours here and records their hashes.
//! 3. Builds `docker/ci-linux.Dockerfile`, clones HEAD fresh inside it (the
//!    repository's `.git`, read only), runs every gate in container mode and
//!    prints the container build's hashes.
//! 4. **Every hash must be equal**: the bootloader byte for byte, the image byte
//!    for byte apart from its 64-byte signature. Builds are reproducible, or red.
//!
//! The container is capped at `$FREALLYOS_CI_LINUX_MEM` (default 7424m, the
//! fleet's 7.25 GB) and named `freally-os-ci-linux`, so `ci-slots.ps1` counts it.

use crate::build;
use crate::util;
use fcore_abi::Flavour;
use std::process::{Command, Stdio};
use std::time::Instant;

const IMAGE: &str = "freally-os-ci-linux";
const CONTAINER: &str = "freally-os-ci-linux";

/// Windows' free memory in GB, through WSL's interop (None off WSL).
fn windows_free_gb() -> Option<f64> {
    let out = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-Command",
            "[math]::Round((Get-CimInstance Win32_OperatingSystem).FreePhysicalMemory/1MB,1)",
        ])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().replace(',', ".").parse().ok()
}

/// The Docker CLI: `docker` when this distribution has Docker's WSL integration,
/// else Windows' `docker.exe` through WSL interop (paths then go as Windows paths).
fn docker() -> Result<(String, bool), String> {
    for (prog, windows) in [("docker", false), ("docker.exe", true)] {
        if Command::new(prog)
            .args(["version", "--format", "{{.Server.Version}}"])
            .output()
            .is_ok_and(|o| o.status.success())
        {
            return Ok((prog.to_string(), windows));
        }
    }
    Err("Docker isn't reachable: start Docker Desktop".into())
}

/// `path` as the Docker daemon sees it.
fn docker_path(path: &std::path::Path, windows: bool) -> Result<String, String> {
    if !windows {
        return Ok(path.display().to_string());
    }
    util::capture(Command::new("wslpath").arg("-w").arg(path)).map(|s| s.trim().to_string())
}

fn lock_held() -> bool {
    let lock = util::target_dir().join("ci.lock");
    let Ok(pid) = std::fs::read_to_string(&lock) else { return false };
    std::path::Path::new("/proc").join(pid.trim()).exists()
}

pub fn ci_linux_cmd(args: &[String]) -> Result<(), String> {
    let start = Instant::now();
    if lock_held() {
        return Err("cargo xtask ci is running; ci-linux runs after it, never beside it".into());
    }
    if let Some(free) = windows_free_gb() {
        if free < 6.0 {
            return Err(format!("Windows has {free} GB free; ci-linux needs 6 GB (the fleet's 4 GB floor)"));
        }
        println!("ci-linux: Windows has {free} GB free");
    }
    let root = util::repo_root();
    let head = util::capture(Command::new("git").current_dir(&root).args(["rev-parse", "HEAD"]))?.trim().to_string();
    let dirty =
        !util::capture(Command::new("git").current_dir(&root).args(["status", "--porcelain"]))?.trim().is_empty();
    if dirty && !args.iter().any(|a| a == "--allow-dirty") {
        return Err("the tree has uncommitted changes; the container builds HEAD, so commit first".into());
    }

    // 2. This machine's hashes.
    let t = Instant::now();
    let mut ours = Vec::new();
    for f in [Flavour::Release, Flavour::Dev, Flavour::E2e] {
        let (efi, content) = build::hashes(&build::build(f)?)?;
        ours.push(format!("hash {} {efi} {content}", f.name()));
    }
    println!("▶ hashes (WSL) … pass ({:.1}s)", t.elapsed().as_secs_f64());

    // 3. The container.
    let t = Instant::now();
    let (docker, windows) = docker()?;
    let docker_ok = util::run(
        Command::new(&docker)
            .current_dir(&root)
            .args(["build", "-q", "-f", "docker/ci-linux.Dockerfile", "-t", IMAGE, "."])
            .stdout(Stdio::null()),
    )?;
    if !docker_ok.success() {
        println!("▶ container image … fail ({:.1}s)", t.elapsed().as_secs_f64());
        return Err("docker build failed".into());
    }
    println!("▶ container image … pass ({:.1}s)", t.elapsed().as_secs_f64());

    let mem = std::env::var("FREALLYOS_CI_LINUX_MEM").unwrap_or_else(|_| "7424m".into());
    let script = format!(
        "set -e; git config --global --add safe.directory '*'; git clone -q /src.git /work/fos && cd /work/fos && git checkout -q {head} \
         && cargo xtask ci --container && cargo xtask hashes --unsigned"
    );
    let t = Instant::now();
    let _ = Command::new(&docker).args(["rm", "-f", CONTAINER]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    let out = Command::new(&docker)
        .args(["run", "--rm", "--name", CONTAINER, "--memory", &mem, "--memory-swap", &mem])
        .arg("-v")
        .arg(format!("{}:/src.git:ro", docker_path(&root.join(".git"), windows)?))
        .args([IMAGE, "bash", "-lc", &script])
        .output()
        .map_err(|e| format!("can't run docker: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    for line in text.lines().filter(|l| !l.starts_with("hash ")) {
        // The container's own gate lines, marked so the report can tell them apart.
        println!("{}", line.replacen("▶ ", "▶ [linux] ", 1).replacen("ci:local:", "container:", 1));
    }
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().take(20).collect::<Vec<_>>().into_iter().rev().collect();
        println!("▶ container gates … fail ({:.1}s)", t.elapsed().as_secs_f64());
        for l in tail {
            println!("    {l}");
        }
        println!("ci:linux: red: the container's gates ({:.1} min)", start.elapsed().as_secs_f64() / 60.0);
        return Err("the container's gates are red".into());
    }
    println!("▶ container gates … pass ({:.1}s)", t.elapsed().as_secs_f64());

    // 4. The hashes must be equal.
    let theirs: Vec<&str> = text.lines().filter(|l| l.starts_with("hash ")).collect();
    let mut same = theirs.len() == ours.len();
    for (a, b) in ours.iter().zip(&theirs) {
        if a != b {
            same = false;
            println!("    WSL:       {a}\n    container: {b}");
        }
    }
    let mins = start.elapsed().as_secs_f64() / 60.0;
    if same {
        println!(
            "▶ reproducible … pass (0.0s)\n    {} flavours: the same bootloader and image bytes in WSL and the container",
            ours.len()
        );
        println!("ci:linux: all gates green, builds reproducible ({mins:.1} min)");
        Ok(())
    } else {
        println!("▶ reproducible … fail (0.0s)");
        println!("ci:linux: red: reproducible ({mins:.1} min)");
        Err("the container built different bytes".into())
    }
}
