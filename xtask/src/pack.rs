// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask pack` and `cargo xtask sign`: the system image, built
//! deterministically (sorted TOC, the commit's time and id, no host paths).

use crate::keys::{self, KeyName};
use crate::util;
use fcore_abi::Flavour;
use fsi::{ImageBuilder, Kind};
use std::path::{Path, PathBuf};
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

/// The fonts every image carries (FR-026), and their licence: stage and the
/// apps get them through the `fonts` grant.
pub const FONTS: &[(&str, Kind)] = &[
    ("NotoSans-Regular.ttf", Kind::Font),
    ("NotoSans-Medium.ttf", Kind::Font),
    ("NotoSans-SemiBold.ttf", Kind::Font),
    ("NotoSansMono-Regular.ttf", Kind::Font),
    ("NotoSansSymbols2-Regular.ttf", Kind::Font),
    ("OFL.txt", Kind::Data),
];

/// The UI scales the image carries the hardware set at: 1x (the VM, and
/// 100 %) and 2x (the OMEN's 150 % and up, drawn from the larger).
pub const HARDWARE_SCALES: [u32; 2] = [1, 2];

/// The hardware set's pieces the OS draws (`fui::hardware::PIECES`), both
/// themes, as FIMGs at `scale`, in one bundle: `dark/<piece>`, `light/<piece>`.
pub fn hardware_bundle(scale: u32) -> Result<Vec<u8>, String> {
    let dir = util::repo_root().join("assets/hardware");
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for theme in ["dark", "light"] {
        for piece in fui::hardware::PIECES {
            let png = std::fs::read(dir.join(theme).join(format!("{piece}.png")))
                .map_err(|e| format!("assets/hardware/{theme}/{piece}.png: {e}"))?;
            let fimg = fimage::fimg::from_png(&png, scale, fui::hardware::RENDER_SCALE, fui::hardware::slice_of(piece))
                .map_err(|e| format!("{theme}/{piece}: {e}"))?;
            files.push((format!("{theme}/{piece}"), fimg));
        }
    }
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    Ok(fimage::bundle::build(&refs))
}

/// Packs the flavour's image, unsigned, with `key_id` set for its signing key.
/// `user` lists each user program: its image path, its kind and its built ELF.
pub fn pack(
    flavour: Flavour,
    out: &Path,
    kernel: &Path,
    user: &[(String, Kind, PathBuf)],
    without: &[&str],
) -> Result<(), String> {
    let (build_id, created) = build_facts()?;
    let public = keys::committed_public(signing_key(flavour))?;
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
    for (file, kind) in FONTS {
        let bytes = std::fs::read(root.join("assets/fonts/noto").join(file)).map_err(|e| format!("{file}: {e}"))?;
        b.add(&format!("/system/fonts/{file}"), *kind, 0, bytes);
    }
    for scale in HARDWARE_SCALES {
        b.add(&format!("/system/assets/hardware-{scale}x.fbnd"), Kind::Asset, 0, hardware_bundle(scale)?);
    }
    let elf = std::fs::read(kernel).map_err(|e| format!("{}: {e} (build it: cargo xtask build)", kernel.display()))?;
    b.add("/system/kernel", Kind::Kernel, 0, elf);
    for (path, kind, elf) in user {
        let bytes = std::fs::read(elf).map_err(|e| format!("{}: {e}", elf.display()))?;
        b.add(path, *kind, 0, bytes);
    }
    for (name, text) in manifests(flavour)?.into_iter().filter(|(n, _)| !without.contains(&n.as_str())) {
        let m = fsvc::parse(&text).map_err(|e| format!("manifests/{name}.svc: {e}"))?;
        if !user.iter().any(|(p, ..)| *p == m.binary) {
            return Err(format!("manifests/{name}.svc names {}, which this image doesn't carry", m.binary));
        }
        b.add(&format!("/system/manifests/{name}.svc"), Kind::Manifest, 0, text.into_bytes());
    }
    let image = b.build(&public)?;
    std::fs::write(out, image).map_err(|e| format!("can't write {}: {e}", out.display()))
}

/// The programs a flavour's image carries: those its manifests name, so a
/// dev- or e2e-only program never reaches a release image (FR-037); less the
/// services `without` names (a test image's).
pub fn programs_for(flavour: Flavour, without: &[&str]) -> Result<Vec<&'static crate::build::UserProgram>, String> {
    let mut binaries = Vec::new();
    for (name, text) in manifests(flavour)?.into_iter().filter(|(n, _)| !without.contains(&n.as_str())) {
        binaries.push(fsvc::parse(&text).map_err(|e| format!("manifests/{name}.svc: {e}"))?.binary);
    }
    Ok(crate::build::PROGRAMS.iter().filter(|p| binaries.iter().any(|b| b == p.path)).collect())
}

/// The manifests (`manifests/*.svc`) whose `flavours` include `flavour`, checked
/// as a set (unique names and binaries, every `after` known, no cycle): each
/// one's name (its file name) and text.
pub fn manifests(flavour: Flavour) -> Result<Vec<(String, String)>, String> {
    let dir = util::repo_root().join("manifests");
    let mut all = Vec::new();
    let mut rd: Vec<_> = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten().collect();
    rd.sort_by_key(|e| e.file_name());
    for e in rd {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("svc") {
            continue;
        }
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let m = fsvc::parse(&text).map_err(|e| format!("manifests/{stem}.svc: {e}"))?;
        if m.name != stem {
            return Err(format!("manifests/{stem}.svc is named {}; the file and the name must match", m.name));
        }
        all.push((m, text));
    }
    let set: Vec<fsvc::Manifest> = all.iter().map(|(m, _)| m.clone()).collect();
    fsvc::check_set(&set)?;
    let want = match flavour {
        Flavour::Release => fsvc::Flavour::Release,
        Flavour::Dev => fsvc::Flavour::Dev,
        Flavour::E2e => fsvc::Flavour::E2e,
    };
    Ok(all.into_iter().filter(|(m, _)| m.flavours.contains(&want)).map(|(m, t)| (m.name, t)).collect())
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
    let user = crate::build::build_user(&programs_for(flavour, &[])?, flavour)?;
    pack(flavour, &out, &crate::build::kernel_path(), &user, &[])?;
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
