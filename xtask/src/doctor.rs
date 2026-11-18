// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask doctor`: is this machine ready to build and run Freally OS?

use crate::util;
use std::fs::OpenOptions;
use std::path::Path;
use std::process::Command;

pub const OVMF_CODE: &str = "/usr/share/OVMF/OVMF_CODE_4M.fd";
pub const OVMF_VARS: &str = "/usr/share/OVMF/OVMF_VARS_4M.fd";

struct Check {
    name: &'static str,
    ok: bool,
    detail: String,
    fix: &'static str,
}

pub fn doctor() -> Result<(), String> {
    let mut checks = Vec::new();

    let kvm = Path::new("/dev/kvm");
    let kvm_rw = OpenOptions::new().read(true).write(true).open(kvm).is_ok();
    checks.push(Check {
        name: "KVM (/dev/kvm)",
        ok: kvm.exists() && kvm_rw,
        detail: if !kvm.exists() {
            "missing".into()
        } else if kvm_rw {
            "present, readable and writable".into()
        } else {
            "present, but this user can't open it".into()
        },
        fix: "In C:\\Users\\<you>\\.wslconfig put nestedVirtualization=true under [wsl2], run `wsl --shutdown`, and check again; then `sudo usermod -aG kvm $USER` and open a new shell.",
    });

    let groups = util::capture(Command::new("id").arg("-nG")).unwrap_or_default();
    let uid = util::capture(Command::new("id").arg("-u")).unwrap_or_default();
    let is_root = uid.trim() == "0";
    checks.push(Check {
        name: "the kvm group",
        ok: is_root || groups.split_whitespace().any(|g| g == "kvm"),
        detail: if is_root { "root (no group needed)".into() } else { groups.trim().to_string() },
        fix: "sudo usermod -aG kvm $USER, then open a new shell.",
    });

    let qemu = util::capture(Command::new("qemu-system-x86_64").arg("--version")).ok();
    checks.push(Check {
        name: "QEMU",
        ok: qemu.is_some(),
        detail: qemu.as_deref().and_then(|v| v.lines().next()).unwrap_or("not found").to_string(),
        fix: "sudo apt install qemu-system-x86",
    });

    for (name, path) in [("OVMF code", OVMF_CODE), ("OVMF variables", OVMF_VARS)] {
        checks.push(Check {
            name,
            ok: Path::new(path).is_file(),
            detail: path.to_string(),
            fix: "sudo apt install ovmf",
        });
    }

    let rustc = util::capture(util::cargo().env_remove("RUSTC").arg("--version")).ok();
    let pinned = std::fs::read_to_string(util::repo_root().join("rust-toolchain.toml")).unwrap_or_default();
    let channel = pinned
        .lines()
        .find_map(|l| l.trim().strip_prefix("channel = "))
        .map(|c| c.trim_matches('"').to_string())
        .unwrap_or_default();
    let rustc_v = util::capture(Command::new("rustc").current_dir(util::repo_root()).arg("-vV")).unwrap_or_default();
    checks.push(Check {
        name: "the pinned Rust nightly",
        ok: rustc.is_some() && rustc_v.contains("nightly"),
        detail: format!(
            "{} (rust-toolchain.toml pins {channel})",
            rustc_v.lines().next().unwrap_or("rustc not found")
        ),
        fix: "Install rustup (https://rustup.rs); the repository's rust-toolchain.toml installs the rest.",
    });

    let sysroot = util::capture(Command::new("rustc").current_dir(util::repo_root()).args(["--print", "sysroot"]))
        .unwrap_or_default();
    let sysroot = sysroot.trim();
    for target in ["x86_64-unknown-uefi", "x86_64-unknown-none"] {
        let dir = Path::new(sysroot).join("lib/rustlib").join(target);
        checks.push(Check {
            name: if target.ends_with("uefi") { "target x86_64-unknown-uefi" } else { "target x86_64-unknown-none" },
            ok: dir.is_dir(),
            detail: dir.display().to_string(),
            fix: "rustup target add x86_64-unknown-uefi x86_64-unknown-none (rust-toolchain.toml lists them)",
        });
    }
    let rust_src = Path::new(sysroot).join("lib/rustlib/src/rust/library/core");
    checks.push(Check {
        name: "rust-src (for -Z build-std)",
        ok: rust_src.is_dir(),
        detail: rust_src.display().to_string(),
        fix: "rustup component add rust-src",
    });

    let target = util::target_dir();
    checks.push(Check {
        name: "CARGO_TARGET_DIR on WSL's disk",
        ok: !target.starts_with("/mnt/"),
        detail: target.display().to_string(),
        fix: "export CARGO_TARGET_DIR=$HOME/.freally-target in ~/.profile (builds across /mnt/c are slow).",
    });

    for (name, prog, fix) in [
        ("Docker (the ci-linux leg)", "docker", "Turn on Docker Desktop's WSL integration for this distribution."),
        ("Node.js (the fleet kits)", "node", "sudo apt install nodejs"),
        ("ffmpeg (showcase videos)", "ffmpeg", "sudo apt install ffmpeg"),
        ("git", "git", "sudo apt install git"),
    ] {
        let found = util::on_path(prog);
        checks.push(Check {
            name,
            ok: found.is_some(),
            detail: found.map(|p| p.display().to_string()).unwrap_or_else(|| "not found".into()),
            fix,
        });
    }
    let chrome = ["google-chrome", "google-chrome-stable", "chromium"].iter().find_map(|p| util::on_path(p));
    checks.push(Check {
        name: "Chrome (the docs-site CDP leg)",
        ok: chrome.is_some(),
        detail: chrome.map(|p| p.display().to_string()).unwrap_or_else(|| "not found".into()),
        fix: "Install google-chrome-stable from dl.google.com's .deb.",
    });

    let keys = util::keys_dir();
    let dev_key = keys.join("dev.key");
    checks.push(Check {
        name: "the dev signing key",
        ok: dev_key.is_file(),
        detail: dev_key.display().to_string(),
        fix: "cargo xtask keygen --dev --staging",
    });

    // The fleet folders `xtask sync-*` may read (owner, 2026-11-17: read only).
    let fleet = util::repo_root().parent().map(Path::to_path_buf).unwrap_or_default();
    for (name, rel) in [("Freally Bugs (the fleet kits)", "Freally Bugs"), ("the playbook (Freally)", "Freally")] {
        let dir = fleet.join(rel);
        checks.push(Check {
            name,
            ok: dir.is_dir(),
            detail: dir.display().to_string(),
            fix: "The fleet folders sit beside this repository in Havoc Software.",
        });
    }

    let mut bad = 0;
    for c in &checks {
        println!("{} {:<36} {}", if c.ok { "✓" } else { "✗" }, c.name, c.detail);
        if !c.ok {
            println!("    fix: {}", c.fix);
            bad += 1;
        }
    }
    if bad == 0 {
        println!("doctor: all {} checks green", checks.len());
        Ok(())
    } else {
        Err(format!("doctor: {bad} of {} checks need attention", checks.len()))
    }
}
