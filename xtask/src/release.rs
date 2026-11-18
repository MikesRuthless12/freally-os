// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask release-local` and `cargo xtask target-clean` (closeout steps 9
//! and 10; THE END-TO-END RELEASE BUILD).
//!
//! `release-local` refuses unless the tree is clean and HEAD carries the tag
//! `vX.Y.Z` of the workspace version. It builds the release flavour (signed with
//! the staging key until `v0.8.0`), verifies the exact files as a release
//! bootloader would, boots them in QEMU/KVM five times, runs the release
//! image's CDP subset, and writes `target/release/Freally-OS-vX.Y.Z/` with
//! `SHA256SUMS.txt` and `RELEASE.json`. Nothing is published: the folder stays
//! on this machine (step 9).
//!
//! `--rehearsal` proves the path on an untagged tree into
//! `Freally-OS-vX.Y.Z-rehearsal/`, named so it can't be mistaken for a release.

use crate::run::{self, VmOptions};
use crate::{build, keys, util};
use fcore_abi::Flavour;
use fjson::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

pub fn releases_dir() -> PathBuf {
    util::repo_root().join("target/release")
}

fn git(args: &[&str]) -> Result<String, String> {
    util::capture(Command::new("git").current_dir(util::repo_root()).args(args)).map(|s| s.trim().to_string())
}

/// Boots the exact files once; the wall time to the title, or why not.
fn boot_once(o: &VmOptions, b: &build::Built) -> Result<f64, String> {
    let start = Instant::now();
    let mut vm = run::start_built(o, b)?;
    let result = loop {
        let log = std::fs::read_to_string(o.com1_log()).unwrap_or_default();
        if log.contains("freally-boot: showing the title") {
            break if log.contains(&format!("verified Freally OS {} (release)", fcore_abi::VERSION)) {
                Ok(start.elapsed().as_secs_f64())
            } else {
                Err("the title showed without a verified release image".to_string())
            };
        }
        if log.contains("ERROR") {
            break Err(format!("COM1: {}", log.lines().find(|l| l.contains("ERROR")).unwrap_or("")));
        }
        if start.elapsed() > Duration::from_secs(60) {
            break Err("no title within 60 s".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = vm.kill();
    let _ = vm.wait();
    result
}

pub fn release_local_cmd(args: &[String]) -> Result<(), String> {
    let rehearsal = args.iter().any(|a| a == "--rehearsal");
    let version = fcore_abi::VERSION;
    let tag = format!("v{version}");
    let head = git(&["rev-parse", "HEAD"])?;
    if !rehearsal {
        if !git(&["status", "--porcelain"])?.is_empty() {
            return Err("the tree isn't clean; release-local builds only committed, tagged trees".into());
        }
        let tagged = git(&["tag", "--points-at", "HEAD"])?;
        if !tagged.lines().any(|t| t.trim() == tag) {
            return Err(format!("HEAD isn't tagged {tag}; tag the rung first (closeout step 8)"));
        }
    }

    // 1–2. Build and sign the release flavour.
    let b = build::build(Flavour::Release)?;
    let efi = std::fs::read(&b.efi).map_err(|e| e.to_string())?;
    let image = std::fs::read(&b.image).map_err(|e| e.to_string())?;

    // 3. Verify the exact files as a release bootloader would: the staging key only.
    let staging = keys::committed_public(keys::KeyName::Staging)?;
    let v = fsi::verify(&image, &[staging]).map_err(|e| format!("the release image fails verification: {}", e.describe()))?;
    if v.header.flavour != Flavour::Release as u16 {
        return Err("the release image isn't the release flavour".into());
    }
    println!("▶ verify … pass (0.0s)\n    the release image verifies against the staging key alone");

    // 4. Boot the exact files five times under KVM.
    let mut o = VmOptions::new(Flavour::Release, "release-check");
    o.headless = true;
    o.port_offset = 300;
    let mut times = Vec::new();
    for i in 1..=5 {
        let t = boot_once(&o, &b).map_err(|e| format!("boot {i} of 5: {e}"))?;
        times.push(t);
    }
    times.sort_by(|a, b| a.total_cmp(b));
    let median = times[2];
    println!(
        "▶ five boots … pass ({:.1}s)\n    QEMU start to the title, median {:.0} ms (firmware included; the boot gate's B0→S4 arrives in Phase 3)",
        times.iter().sum::<f64>(),
        median * 1000.0
    );

    // 5. The release image's CDP subset (QMP only): the title, pixel for pixel.
    crate::cdp::cdp_cmd(&["--os".into(), "--only=boot.release".into()])?;

    // 6. The folder, checksums and RELEASE.json.
    let name = if rehearsal { format!("Freally-OS-{tag}-rehearsal") } else { format!("Freally-OS-{tag}") };
    let dir = releases_dir().join(&name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let files = [("BOOTX64.EFI", &efi), ("SYSTEM_A.FSI", &image)];
    let mut sums = String::new();
    for (n, data) in files {
        std::fs::write(dir.join(n), data).map_err(|e| e.to_string())?;
        sums.push_str(&format!("{}  {n}\n", util::hex(&fcrypto::sha2::sha256(data))));
    }
    std::fs::write(dir.join("SHA256SUMS.txt"), &sums).map_err(|e| e.to_string())?;
    let created = git(&["log", "-1", "--format=%cI"])?;
    let release = Value::obj([
        ("product", "Freally OS".into()),
        ("version", version.into()),
        ("tag", tag.clone().into()),
        ("rehearsal", rehearsal.into()),
        ("commit", head.into()),
        ("committed", created.into()),
        ("flavour", "release".into()),
        ("signed_with", "staging".into()),
        ("layout", "files (\\EFI\\BOOT\\BOOTX64.EFI, \\FREALLY\\SYSTEM_A.FSI)".into()),
        ("boot_to_title_ms_median", ((median * 1000.0).round() as i64).into()),
        ("files", Value::Arr(files.iter().map(|(n, d)| Value::obj([("name", (*n).into()), ("bytes", d.len().into()), ("sha256", util::hex(&fcrypto::sha2::sha256(d)).into())])).collect())),
    ]);
    std::fs::write(dir.join("RELEASE.json"), release.to_json() + "\n").map_err(|e| e.to_string())?;
    println!("release-local: {} ({})", dir.display(), if rehearsal { "a rehearsal, not a release" } else { "kept off GitHub" });
    print!("{sums}");
    Ok(())
}

/// Every file `SHA256SUMS.txt` lists is present and matches.
pub fn verify_release_dir(dir: &Path) -> Result<usize, String> {
    let sums = std::fs::read_to_string(dir.join("SHA256SUMS.txt")).map_err(|_| format!("{}: no SHA256SUMS.txt", dir.display()))?;
    let mut n = 0;
    for line in sums.lines().filter(|l| !l.trim().is_empty()) {
        let (hash, name) = line.split_once("  ").ok_or_else(|| format!("{}: a bad SHA256SUMS line", dir.display()))?;
        let data = std::fs::read(dir.join(name)).map_err(|_| format!("{}: {name} is missing", dir.display()))?;
        if util::hex(&fcrypto::sha2::sha256(&data)) != hash {
            return Err(format!("{}: {name} doesn't match its checksum", dir.display()));
        }
        n += 1;
    }
    Ok(n)
}

pub fn target_clean_cmd(_args: &[String]) -> Result<(), String> {
    // Refuse if any kept release is incomplete: deleting the cache must never cost one.
    let releases = releases_dir();
    let mut kept = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&releases) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with("Freally-OS-v") && e.path().is_dir() {
                let files = verify_release_dir(&e.path())?;
                kept.push(format!("{n} ({files} files verified)"));
            }
        }
    }
    // The repository's target/: everything but the release folders.
    let target = util::repo_root().join("target");
    if let Ok(rd) = std::fs::read_dir(&target) {
        for e in rd.flatten() {
            if e.file_name() == "release" {
                if let Ok(inner) = std::fs::read_dir(e.path()) {
                    for f in inner.flatten() {
                        if !f.file_name().to_string_lossy().starts_with("Freally-OS-v") {
                            remove(&f.path())?;
                        }
                    }
                }
            } else {
                remove(&e.path())?;
            }
        }
    }
    // The WSL build cache, all of it (the next build is a cold build).
    let cache = util::target_dir();
    let freed = du(&cache);
    if cache.starts_with(util::repo_root()) {
        return Err("CARGO_TARGET_DIR is inside the repository; refusing to delete it".into());
    }
    let _ = std::fs::remove_dir_all(&cache);
    println!("target-clean: pruned {} ({:.1} GB); kept {}", cache.display(), freed as f64 / 1e9, if kept.is_empty() { "no releases".to_string() } else { kept.join(", ") });
    Ok(())
}

fn remove(p: &Path) -> Result<(), String> {
    if p.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) }.map_err(|e| format!("{}: {e}", p.display()))
}

fn du(p: &Path) -> u64 {
    let mut files = Vec::new();
    util::walk_all(p, &mut files);
    files.iter().filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len()).sum()
}
