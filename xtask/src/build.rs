// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask build --flavour release|dev|e2e`: the bootloader for its flavour,
//! the image packed and signed. Output: `$CARGO_TARGET_DIR/freally/<flavour>/`
//! with `BOOTX64.EFI` and `SYSTEM_A.FSI`.

use crate::pack;
use crate::util;
use fcore_abi::Flavour;
use std::path::PathBuf;

pub struct Built {
    pub dir: PathBuf,
    pub efi: PathBuf,
    pub image: PathBuf,
}

/// The flags every OS build uses, so builds are reproducible: no host path
/// reaches a binary, and the PE timestamp comes from the contents.
pub fn reproducible_rustflags() -> String {
    let root = util::repo_root();
    let home = util::home();
    [
        format!("--remap-path-prefix={}=/freally-os", root.display()),
        format!("--remap-path-prefix={}=/cargo", home.join(".cargo").display()),
        format!("--remap-path-prefix={}=/rustup", home.join(".rustup").display()),
        "-Clink-arg=/Brepro".to_string(),
        // No PDB and no debug directory: its signature would hash each machine's paths.
        "-Clink-arg=/DEBUG:NONE".to_string(),
        "-Cdebuginfo=0".to_string(),
    ]
    .join("\x1f")
}

/// Builds and signs a flavour.
pub fn build(flavour: Flavour) -> Result<Built, String> {
    build_with(flavour, true)
}

/// `signed = false` leaves the signature empty: an image no bootloader accepts,
/// made only where there are no keys (the ci-linux container) to compare hashes.
pub fn build_with(flavour: Flavour, signed: bool) -> Result<Built, String> {
    let out = util::target_dir().join("freally").join(flavour.name());
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;

    // 1. The bootloader, with this flavour's keys.
    util::run_ok(util::cargo().env("CARGO_ENCODED_RUSTFLAGS", reproducible_rustflags()).env_remove("RUSTFLAGS").args(
        [
            "build",
            "--quiet",
            "--package",
            "freally-boot",
            "--target",
            "x86_64-unknown-uefi",
            "--release",
            "--features",
            &format!("flavour-{}", flavour.name()),
        ],
    ))?;
    let efi_src = util::target_dir().join("x86_64-unknown-uefi/release/freally-boot.efi");
    let efi = out.join("BOOTX64.EFI");
    std::fs::copy(&efi_src, &efi).map_err(|e| format!("can't copy {}: {e}", efi_src.display()))?;

    // 2. The image, packed and signed.
    let image = out.join("SYSTEM_A.FSI");
    pack::pack(flavour, &image)?;
    if signed {
        pack::sign(&image, pack::signing_key(flavour))?;
    }
    Ok(Built { dir: out, efi, image })
}

pub fn build_cmd(args: &[String]) -> Result<(), String> {
    let flavour = pack::flavour_arg(args)?;
    let b = build(flavour)?;
    let efi = std::fs::read(&b.efi).map_err(|e| e.to_string())?;
    let img = std::fs::read(&b.image).map_err(|e| e.to_string())?;
    println!("build: {} → {}", flavour.name(), b.dir.display());
    println!("  BOOTX64.EFI   {:>9} bytes  sha256 {}", efi.len(), util::hex(&fcrypto::sha2::sha256(&efi)));
    println!("  SYSTEM_A.FSI  {:>9} bytes  sha256 {}", img.len(), util::hex(&fcrypto::sha2::sha256(&img)));
    Ok(())
}

/// The reproducibility hashes of a build: the bootloader's SHA-256, and the
/// image's with its 64-byte signature zeroed (the only part made with a secret).
pub fn hashes(b: &Built) -> Result<(String, String), String> {
    let efi = std::fs::read(&b.efi).map_err(|e| e.to_string())?;
    let mut img = std::fs::read(&b.image).map_err(|e| e.to_string())?;
    if img.len() >= fsi::HEADER_LEN {
        img[fsi::SIGNED_LEN..fsi::HEADER_LEN].fill(0);
    }
    Ok((util::hex(&fcrypto::sha2::sha256(&efi)), util::hex(&fcrypto::sha2::sha256(&img))))
}

/// `cargo xtask hashes [--unsigned]`: one `hash <flavour> <efi> <content>` line per flavour.
pub fn hashes_cmd(args: &[String]) -> Result<(), String> {
    let signed = !args.iter().any(|a| a == "--unsigned");
    for f in [Flavour::Release, Flavour::Dev, Flavour::E2e] {
        let (efi, content) = hashes(&build_with(f, signed)?)?;
        println!("hash {} {efi} {content}", f.name());
    }
    Ok(())
}
