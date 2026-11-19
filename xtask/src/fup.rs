// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask fup --version V [--flavour F] [--oldest V] [--notes FILE]
//! [--crash] [--out PATH]` (TASK-110, FR-050): an update package (PRD § 3):
//! the flavour's image packed as version V, signed with the flavour's key,
//! with its release notes. `--crash` makes one whose system never starts
//! (`@update.fallback`).

use crate::{build, keys, pack, util};
use std::path::{Path, PathBuf};

pub fn fup_cmd(args: &[String]) -> Result<(), String> {
    let flavour = pack::flavour_arg(args)?;
    let version = util::flag(args, "--version").ok_or("--version <the version it brings>")?;
    let notes = match util::flag(args, "--notes") {
        Some(p) => std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?,
        None => format!("# Freally OS {version}\n"),
    };
    let out = util::flag(args, "--out").map(PathBuf::from).unwrap_or_else(|| {
        util::target_dir().join("freally").join("updates").join(format!("{}-{version}.fup", flavour.name()))
    });
    let crash = args.iter().any(|a| a == "--crash");
    make(flavour, version, util::flag(args, "--oldest").unwrap_or(""), &notes, crash, &out)?;
    println!("fup: {} ({} {version})", out.display(), flavour.name());
    Ok(())
}

/// An update package written to `out`.
pub fn make(
    flavour: fcore_abi::Flavour,
    version: &str,
    oldest: &str,
    notes: &str,
    crash: bool,
    out: &Path,
) -> Result<(), String> {
    if flavour == fcore_abi::Flavour::Release {
        // A release update is signed with the release key, from its drive,
        // once the key ceremony has made it (TASK-109).
        return Err("a release update is signed in the release pipeline, with the release key".into());
    }
    let image = build::update_image(flavour, version, if crash { &["crash-ud2"] } else { &[] })?;
    let bytes = std::fs::read(&image).map_err(|e| format!("{}: {e}", image.display()))?;
    let seed = keys::load_seed(pack::signing_key(flavour))?;
    let f = fsi::fup::make(oldest, notes, &bytes, &seed)?;
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(out, f).map_err(|e| format!("{}: {e}", out.display()))
}

/// `version` with its patch number raised by `by` (0.4.0 → 0.4.1).
pub fn bump(version: &str, by: u64) -> String {
    let mut parts: Vec<u64> =
        version.split(['-', '+']).next().unwrap_or("0").split('.').map(|p| p.parse().unwrap_or(0)).collect();
    parts.resize(3, 0);
    parts[2] += by;
    format!("{}.{}.{}", parts[0], parts[1], parts[2])
}

/// The packages `@update.*` use (TASK-110), put in `/system/updates` on the
/// disk `to` before it boots: the next version; a copy of it with one byte of
/// its image changed (a reflink and one block, so it costs nothing); its
/// header with the key's id rubbed out (signed by no key the system
/// trusts); and a version whose system never starts.
pub fn stock(to: &Path, flavour: fcore_abi::Flavour) -> Result<(), String> {
    use svc_ffs::inode::FileKind;
    use svc_ffs::subvol::SYSTEM;
    let dir = util::target_dir().join("freally").join("updates");
    let (next, broken) = (bump(fcore_abi::VERSION, 1), bump(fcore_abi::VERSION, 2));
    let good_name = format!("{}-{next}.fup", flavour.name());
    let crash_name = format!("{}-{broken}-crash.fup", flavour.name());
    let notes = |v: &str| format!("# Freally OS {v}\n\nA test update ({}).\n", flavour.name());
    make(flavour, &next, "", &notes(&next), false, &dir.join(&good_name))?;
    make(flavour, &broken, "", &notes(&broken), true, &dir.join(&crash_name))?;
    let fresh = |v: &mut svc_ffs::volume::Volume<crate::carry::Disk>, at: u64, name: &str| {
        if v.lookup(SYSTEM, at, name)?.is_some() {
            v.unlink(SYSTEM, at, name)?;
        }
        v.create(SYSTEM, at, name, FileKind::File)
    };
    let good = std::fs::read(dir.join(&good_name)).map_err(|e| e.to_string())?;
    let head = fsi::fup::HEADER + u32::from_le_bytes([good[12], good[13], good[14], good[15]]) as usize;
    let mut foreign = good[..head].to_vec();
    foreign[152..160].fill(0);
    crate::carry::in_system(to, "updates", |v, at| {
        let g = fresh(v, at, &good_name)?;
        crate::carry::write_whole(v, g, &good)?;
        if v.lookup(SYSTEM, at, "changed.fup")?.is_some() {
            v.unlink(SYSTEM, at, "changed.fup")?;
        }
        let c = v.clone_file(SYSTEM, g, at, "changed.fup")?;
        let off = good.len() as u64 - 100;
        v.write(SYSTEM, c, off, &[good[off as usize] ^ 1])?;
        let f = fresh(v, at, "foreign.fup")?;
        v.write(SYSTEM, f, 0, &foreign)
    })?;
    drop(good);
    let crash = std::fs::read(dir.join(&crash_name)).map_err(|e| e.to_string())?;
    crate::carry::in_system(to, "updates", |v, at| {
        let f = fresh(v, at, &crash_name)?;
        crate::carry::write_whole(v, f, &crash)
    })?;
    println!("fup: {good_name}, changed.fup, foreign.fup and {crash_name} in /system/updates on {}", to.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_patch_number_goes_up() {
        assert_eq!(super::bump("0.4.0", 1), "0.4.1");
        assert_eq!(super::bump("0.8.0-rc", 2), "0.8.2");
        assert_eq!(super::bump("1", 1), "1.0.1");
    }
}
