// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask pack` and `cargo xtask sign`: the system image, built
//! deterministically (sorted TOC, the commit's time and id, no host paths).

use crate::keys::{self, KeyName};
use crate::util;
use fcore_abi::Flavour;
use fsi::{ImageBuilder, Kind};
use std::path::Path;
use std::process::Command;

/// The key that signs a flavour's image. Release uses staging until `v0.8.0`.
pub fn signing_key(flavour: Flavour) -> KeyName {
    match flavour {
        Flavour::Release => KeyName::Staging,
        Flavour::Dev | Flavour::E2e => KeyName::Dev,
    }
}

pub fn parse_flavour(s: &str) -> Result<Flavour, String> {
    match s {
        "release" => Ok(Flavour::Release),
        "dev" => Ok(Flavour::Dev),
        "e2e" => Ok(Flavour::E2e),
        _ => Err(format!("unknown flavour {s:?} (release, dev or e2e)")),
    }
}

/// The flavour named by `--flavour X` in `args` (default dev).
pub fn flavour_arg(args: &[String]) -> Result<Flavour, String> {
    match args.iter().position(|a| a == "--flavour") {
        Some(i) => parse_flavour(args.get(i + 1).ok_or("--flavour needs a value")?),
        None => Ok(Flavour::Dev),
    }
}

/// The commit this build comes from: its id and its time (or `SOURCE_DATE_EPOCH`).
pub fn build_facts() -> Result<([u8; 20], u64), String> {
    let root = util::repo_root();
    let id = util::capture(Command::new("git").current_dir(&root).args(["rev-parse", "HEAD"]))?;
    let id: [u8; 20] = util::unhex(id.trim())?.try_into().map_err(|_| "HEAD isn't a SHA-1".to_string())?;
    let time = match std::env::var("SOURCE_DATE_EPOCH") {
        Ok(v) => v.trim().parse().map_err(|_| "SOURCE_DATE_EPOCH isn't a number".to_string())?,
        Err(_) => util::capture(Command::new("git").current_dir(&root).args(["log", "-1", "--format=%ct"]))?
            .trim()
            .parse()
            .map_err(|_| "git log gave no commit time".to_string())?,
    };
    Ok((id, time))
}

/// Packs the flavour's image, unsigned, with `key_id` set for its signing key.
pub fn pack(flavour: Flavour, out: &Path) -> Result<(), String> {
    let (build_id, created) = build_facts()?;
    let public = keys::load_public(signing_key(flavour))?;
    let root = util::repo_root();
    let notice = std::fs::read(root.join("NOTICE")).map_err(|e| format!("NOTICE: {e}"))?;
    let mut b = ImageBuilder::new(flavour as u16, fcore_abi::VERSION, build_id, created);
    b.add(
        "/system/version",
        Kind::Data,
        0,
        format!("Freally OS {} ({})\n", fcore_abi::VERSION, flavour.name()).into_bytes(),
    );
    b.add("/system/NOTICE", Kind::Data, 0, notice);
    let image = b.build(&public)?;
    std::fs::write(out, image).map_err(|e| format!("can't write {}: {e}", out.display()))
}

/// Signs the image at `path` in place with `key`.
pub fn sign(path: &Path, key: KeyName) -> Result<(), String> {
    let seed = keys::load_seed(key)?;
    let mut image = std::fs::read(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    fsi::sign(&mut image, &seed).map_err(|e| format!("can't sign {}: {}", path.display(), e.describe()))?;
    std::fs::write(path, &image).map_err(|e| e.to_string())
}

pub fn pack_cmd(args: &[String]) -> Result<(), String> {
    let flavour = flavour_arg(args)?;
    let out = match args.iter().position(|a| a == "--out") {
        Some(i) => args.get(i + 1).ok_or("--out needs a path")?.into(),
        None => util::target_dir().join("freally").join(flavour.name()).join("SYSTEM_A.FSI"),
    };
    let out: std::path::PathBuf = out;
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    pack(flavour, &out)?;
    println!("pack: {} (unsigned, for the {} key)", out.display(), signing_key(flavour).as_str());
    Ok(())
}

pub fn sign_cmd(args: &[String]) -> Result<(), String> {
    let path = args.first().ok_or("cargo xtask sign <image> --key dev|staging")?;
    let key = match args.iter().position(|a| a == "--key").and_then(|i| args.get(i + 1)).map(String::as_str) {
        Some("dev") => KeyName::Dev,
        Some("staging") => KeyName::Staging,
        Some("release") => return Err("release signing comes with the key ceremony (Phase 7)".into()),
        _ => return Err("--key dev|staging".into()),
    };
    sign(Path::new(path), key)?;
    println!("sign: {path} signed with the {} key", key.as_str());
    Ok(())
}
