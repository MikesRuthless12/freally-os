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
        "-Cdebuginfo=0".to_string(),
    ]
    .join("\x1f")
}

/// The bootloader's flags: lld-link with a content-derived timestamp, and no PDB
/// or debug directory (its signature would hash each machine's paths).
fn boot_rustflags() -> String {
    [reproducible_rustflags(), "-Clink-arg=/Brepro".into(), "-Clink-arg=/DEBUG:NONE".into()].join("\x1f")
}

/// The kernel's flags: a static ELF at its linked address, frame pointers kept
/// (fault reports walk them), no build id.
fn kernel_rustflags() -> String {
    let script = util::repo_root().join("kernel/linker.ld");
    [
        reproducible_rustflags(),
        format!("-Clink-arg=-T{}", script.display()),
        "-Clink-arg=--build-id=none".into(),
        // Every input section must be placed by the script: an orphan (a `.got`, once)
        // lands between the script's symbols, and the kernel's own page tables miss it.
        "-Clink-arg=--orphan-handling=error".into(),
        "-Crelocation-model=static".into(),
        "-Cforce-frame-pointers=yes".into(),
    ]
    .join("\x1f")
}

/// User space's flags: a static ELF at `USER_IMAGE_BASE`, each segment on its
/// own pages (W^X per page), frame pointers kept (fault reports walk them).
fn user_rustflags() -> String {
    [
        reproducible_rustflags(),
        "-Crelocation-model=static".into(),
        "-Cforce-frame-pointers=yes".into(),
        format!("-Clink-arg=--image-base={:#x}", fcore_abi::layout::USER_IMAGE_BASE),
        "-Clink-arg=--build-id=none".into(),
        "-Clink-arg=-zseparate-loadable-segments".into(),
        "-Clink-arg=-zmax-page-size=4096".into(),
    ]
    .join("")
}

/// The user-space target (a JSON spec; core and alloc are built for it with `-Z build-std`).
pub fn user_target() -> PathBuf {
    util::repo_root().join("targets/x86_64-freally.json")
}

/// The cargo arguments every user-space build or check needs.
pub fn user_target_args() -> Vec<String> {
    vec![
        "--target".into(),
        user_target().display().to_string(),
        "-Zbuild-std=core,alloc,compiler_builtins".into(),
        "-Zbuild-std-features=compiler-builtins-mem".into(),
        "-Zjson-target-spec".into(),
    ]
}

/// A user program: its package, its binary, and where it goes in the image.
pub struct UserProgram {
    pub package: &'static str,
    pub bin: &'static str,
    pub path: &'static str,
    pub kind: fsi::Kind,
}

/// Every image has these.
pub const PROGRAMS: &[UserProgram] = &[
    UserProgram { package: "svc-conductor", bin: "conductor", path: "/system/conductor", kind: fsi::Kind::Conductor },
    UserProgram { package: "svc-logger", bin: "logger", path: "/system/services/logger", kind: fsi::Kind::Service },
    UserProgram { package: "svc-devmgr", bin: "devmgr", path: "/system/services/devmgr", kind: fsi::Kind::Service },
    UserProgram { package: "drv-gop-fb", bin: "drv-gop-fb", path: "/system/drivers/gop-fb", kind: fsi::Kind::Driver },
    UserProgram { package: "drv-i8042", bin: "drv-i8042", path: "/system/drivers/i8042", kind: fsi::Kind::Driver },
    UserProgram { package: "svc-stage", bin: "stage", path: "/system/services/stage", kind: fsi::Kind::Service },
    UserProgram {
        package: "drv-uart16550",
        bin: "drv-uart16550",
        path: "/system/drivers/uart16550",
        kind: fsi::Kind::Driver,
    },
];

/// Test images (`cargo xtask test`) add these.
pub const TEST_PROGRAMS: &[UserProgram] =
    &[UserProgram { package: "ftest", bin: "utest", path: "/system/test/utest", kind: fsi::Kind::App }];

/// Builds user programs for `flavour` (each package's `flavour-*` feature:
/// what only one flavour may carry is compiled out of the others); returns
/// each one's image path, kind and built ELF.
pub fn build_user(programs: &[&UserProgram], flavour: Flavour) -> Result<Vec<(String, fsi::Kind, PathBuf)>, String> {
    let mut c = util::cargo();
    c.env("CARGO_ENCODED_RUSTFLAGS", user_rustflags()).env_remove("RUSTFLAGS");
    c.args(["build", "--quiet", "--release"]).args(user_target_args());
    let mut features = Vec::new();
    for p in programs {
        c.args(["--package", p.package, "--bin", p.bin]);
        features.push(format!("{}/flavour-{}", p.package, flavour.name()));
    }
    c.args(["--features", &features.join(",")]);
    util::run_ok(&mut c)?;
    let dir = util::target_dir().join("x86_64-freally/release");
    Ok(programs.iter().map(|p| (p.path.to_string(), p.kind, dir.join(p.bin))).collect())
}

/// Where the kernel ELF lands.
pub fn kernel_path() -> PathBuf {
    util::target_dir().join("x86_64-unknown-none/release/freally-core")
}

/// Builds Freally Core for a flavour, with any extra features (`ktest`, test-only crashes).
pub fn build_kernel(flavour: Flavour, extra: &[&str]) -> Result<PathBuf, String> {
    let mut features = format!("flavour-{}", flavour.name());
    for f in extra {
        features.push(',');
        features.push_str(f);
    }
    util::run_ok(util::cargo().env("CARGO_ENCODED_RUSTFLAGS", kernel_rustflags()).env_remove("RUSTFLAGS").args([
        "build",
        "--quiet",
        "--package",
        "freally-core",
        "--target",
        "x86_64-unknown-none",
        "--release",
        "--features",
        &features,
    ]))?;
    Ok(kernel_path())
}

/// Builds and signs a flavour.
pub fn build(flavour: Flavour) -> Result<Built, String> {
    build_with(flavour, true)
}

/// `signed = false` leaves the signature empty: an image no bootloader accepts,
/// made only where there are no keys (the ci-linux container) to compare hashes.
pub fn build_with(flavour: Flavour, signed: bool) -> Result<Built, String> {
    build_full(flavour, signed, &[], &[])
}

/// The whole flavour: bootloader, kernel (with `kernel_features` added), image.
/// `without` names services a test image leaves out (`@boot.title`'s image
/// has no stage, so the title stays on the screen).
pub fn build_full(flavour: Flavour, signed: bool, kernel_features: &[&str], without: &[&str]) -> Result<Built, String> {
    let out = util::target_dir().join("freally").join(flavour.name());
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;

    // 1. The bootloader, with this flavour's keys.
    util::run_ok(util::cargo().env("CARGO_ENCODED_RUSTFLAGS", boot_rustflags()).env_remove("RUSTFLAGS").args([
        "build",
        "--quiet",
        "--package",
        "freally-boot",
        "--target",
        "x86_64-unknown-uefi",
        "--release",
        "--features",
        &format!("flavour-{}", flavour.name()),
    ]))?;
    let efi_src = util::target_dir().join("x86_64-unknown-uefi/release/freally-boot.efi");
    let efi = out.join("BOOTX64.EFI");
    std::fs::copy(&efi_src, &efi).map_err(|e| format!("can't copy {}: {e}", efi_src.display()))?;

    // 2. Freally Core.
    let kernel = build_kernel(flavour, kernel_features)?;

    // 3. User space: conductor, and the test program in test images.
    let mut programs = pack::programs_for(flavour, without)?;
    if kernel_features.contains(&"ktest") {
        programs.extend(TEST_PROGRAMS);
    }
    let user = build_user(&programs, flavour)?;

    // 4. The image, packed and signed.
    let image = out.join("SYSTEM_A.FSI");
    pack::pack(flavour, &image, &kernel, &user, without)?;
    if signed {
        pack::sign(&image, pack::signing_key(flavour))?;
    }
    Ok(Built { dir: out, efi, image })
}

pub fn build_cmd(args: &[String]) -> Result<(), String> {
    let flavour = pack::flavour_arg(args)?;
    if args.iter().any(|a| a == "--crash-test") {
        let b = crash_test(flavour)?;
        println!(
            "build: {} crash test (a slot-B image that panics before the desktop) → {}",
            flavour.name(),
            b.display()
        );
        return Ok(());
    }
    let b = build(flavour)?;
    let efi = std::fs::read(&b.efi).map_err(|e| e.to_string())?;
    let img = std::fs::read(&b.image).map_err(|e| e.to_string())?;
    println!("build: {} → {}", flavour.name(), b.dir.display());
    println!("  BOOTX64.EFI   {:>9} bytes  sha256 {}", efi.len(), util::hex(&fcrypto::sha2::sha256(&efi)));
    println!("  SYSTEM_A.FSI  {:>9} bytes  sha256 {}", img.len(), util::hex(&fcrypto::sha2::sha256(&img)));
    Ok(())
}

/// A slot-B image that panics in Freally Core, before conductor (TASK-045's
/// `@boot.fallback`): `target/freally/<flavour>-crash-test/SYSTEM_B.FSI`.
pub fn crash_test(flavour: Flavour) -> Result<PathBuf, String> {
    let b = build_full(flavour, true, &["crash-ud2"], &[])?;
    let dir = util::target_dir().join("freally").join(format!("{}-crash-test", flavour.name()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let out = dir.join("SYSTEM_B.FSI");
    std::fs::copy(&b.image, &out).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(out)
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
