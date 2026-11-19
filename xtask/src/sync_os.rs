// SPDX-License-Identifier: Apache-2.0
//! The OS's own renders (TASK-052, FR-025): the playbook's
//! `Freally/renders/os/{dark,light}` (from `Freally/blender/os.py`) in the OS.
//!
//! - `cargo xtask sync-os` copies each theme's PNGs and `os.json` byte for
//!   byte into `assets/os/` (read-only on the source) and seals the copy in
//!   `assets/os/MANIFEST`.
//! - The **os set** gate: the copy is what its MANIFEST says; on a machine
//!   with the playbook, it is the playbook's.
//! - `os_bundle(pct)`: every piece of both themes but the wallpaper as FIMG
//!   at `pct` percent (each piece's render scale and 9-slice insets from
//!   `os.json`), one bundle for each scale the OS offers (TASK-090A).
//! - `wallpaper_bundle()`: both themes' wallpapers whole, at the size they
//!   were rendered (2560 × 1600); stage brings them down to the screen with
//!   an area filter, never up on a screen no larger. Beside the studio wall
//!   (`wallpaper`), each other look Settings offers (TASK-104):
//!   `wallpaper-<look>.png`, rendered as the wall is.

use crate::util;
use fjson::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const THEMES: [&str; 2] = ["dark", "light"];

fn source() -> PathBuf {
    util::repo_root().parent().map(|p| p.join("Freally/renders/os")).unwrap_or_default()
}

fn copy_dir() -> PathBuf {
    util::repo_root().join("assets/os")
}

/// What a set holds: `os.json`, and each theme's PNGs → SHA-256.
fn manifest_of(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut m = BTreeMap::new();
    let mut put = |rel: String| -> Result<(), String> {
        let bytes = std::fs::read(root.join(&rel)).map_err(|e| format!("{rel}: {e}"))?;
        m.insert(rel, util::hex(&fcrypto::sha2::sha256(&bytes)));
        Ok(())
    };
    if root.join("os.json").exists() {
        put(String::from("os.json"))?;
    }
    for theme in THEMES {
        let Ok(entries) = std::fs::read_dir(root.join(theme)) else { continue };
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .filter(|n| n.ends_with(".png"))
            .collect();
        names.sort();
        for n in names {
            put(format!("{theme}/{n}"))?;
        }
    }
    Ok(m)
}

fn manifest_text(m: &BTreeMap<String, String>) -> String {
    m.iter().map(|(path, hash)| format!("{hash}  {path}\n")).collect()
}

pub fn sync_os_cmd(_args: &[String]) -> Result<(), String> {
    let src = source();
    let m = manifest_of(&src)?;
    if !m.contains_key("os.json") {
        return Err(format!("no OS renders at {} (run Freally/blender/os.py)", src.display()));
    }
    let dir = copy_dir();
    let _ = std::fs::remove_dir_all(&dir);
    for path in m.keys() {
        let to = dir.join(path);
        std::fs::create_dir_all(to.parent().unwrap_or(&dir)).map_err(|e| e.to_string())?;
        std::fs::copy(src.join(path), &to).map_err(|e| format!("{path}: {e}"))?;
    }
    std::fs::write(dir.join("MANIFEST"), manifest_text(&m)).map_err(|e| e.to_string())?;
    println!("sync-os: {} files copied and sealed", m.len());
    Ok(())
}

/// The **os set** gate.
pub fn gate() -> Result<String, String> {
    let dir = copy_dir();
    let sealed = std::fs::read_to_string(dir.join("MANIFEST")).map_err(|e| format!("assets/os/MANIFEST: {e}"))?;
    if manifest_text(&manifest_of(&dir)?) != sealed {
        return Err("assets/os isn't what its MANIFEST says (edited, added or missing): cargo xtask sync-os".into());
    }
    let n = sealed.lines().count();
    let src = source();
    if src.join("os.json").exists() {
        if manifest_text(&manifest_of(&src)?) != sealed {
            return Err("assets/os isn't the playbook's renders: cargo xtask sync-os".into());
        }
        return Ok(format!("{n} files, sealed, and the playbook's renders"));
    }
    Ok(format!("{n} files, sealed"))
}

/// Each piece's render scale and 9-slice insets (at that scale), from `os.json`.
pub fn pieces() -> Result<Vec<(String, u32, [u32; 4])>, String> {
    let path = copy_dir().join("os.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v = fjson::parse(&text).map_err(|e| format!("os.json at byte {}: {}", e.at, e.what))?;
    let Some(Value::Obj(list)) = v.get("pieces") else { return Err("os.json has no pieces".into()) };
    let mut out = Vec::new();
    for (name, p) in list {
        let scale = p.get("scale").and_then(Value::as_i64).ok_or(format!("{name}: no scale"))? as u32;
        let s: Vec<u32> = p
            .get("slice")
            .and_then(Value::as_array)
            .unwrap_or_default()
            .iter()
            .filter_map(Value::as_i64)
            .map(|v| v as u32)
            .collect();
        let slice = [0, 1, 2, 3].map(|i| s.get(i).copied().unwrap_or(0));
        out.push((name.clone(), scale, slice));
    }
    Ok(out)
}

/// Both themes' pieces but the wallpaper as FIMGs at `pct` percent, in one
/// bundle: `dark/<piece>`, `light/<piece>`.
pub fn os_bundle(pct: u32) -> Result<Vec<u8>, String> {
    bundle_of(|piece| piece != "wallpaper", |_| pct)
}

/// Both themes' wallpapers, each at its render's own size: `dark/wallpaper`,
/// `light/wallpaper`.
pub fn wallpaper_bundle() -> Result<Vec<u8>, String> {
    let (_, scale, slice) = pieces()?.into_iter().find(|(p, ..)| p == "wallpaper").ok_or("os.json has no wallpaper")?;
    let dir = copy_dir();
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for theme in THEMES {
        let mut names = vec![String::from("wallpaper")];
        names.extend(looks(&dir.join(theme))?);
        for piece in names {
            let png = std::fs::read(dir.join(theme).join(format!("{piece}.png")))
                .map_err(|e| format!("assets/os/{theme}/{piece}.png: {e}"))?;
            let fimg = fimage::fimg::from_png(&png, scale * 100, scale, slice)
                .map_err(|e| format!("{theme}/{piece} at {} %: {e}", scale * 100))?;
            files.push((format!("{theme}/{piece}"), fimg));
        }
    }
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    Ok(fimage::bundle::build(&refs))
}

/// The other looks rendered for a theme: `wallpaper-<look>`, sorted.
fn looks(theme_dir: &Path) -> Result<Vec<String>, String> {
    let mut v: Vec<String> = std::fs::read_dir(theme_dir)
        .map_err(|e| format!("{}: {e}", theme_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .filter_map(|n| {
            n.strip_prefix("wallpaper-").and_then(|n| n.strip_suffix(".png")).map(|l| format!("wallpaper-{l}"))
        })
        .collect();
    v.sort();
    Ok(v)
}

/// The pieces `which` picks, both themes, each as FIMG at the percent `at`
/// gives for its render scale.
fn bundle_of(which: impl Fn(&str) -> bool, at: impl Fn(u32) -> u32) -> Result<Vec<u8>, String> {
    let dir = copy_dir();
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for theme in THEMES {
        for (piece, render_scale, slice) in pieces()?.into_iter().filter(|(p, ..)| which(p)) {
            let pct = at(render_scale);
            let png = std::fs::read(dir.join(theme).join(format!("{piece}.png")))
                .map_err(|e| format!("assets/os/{theme}/{piece}.png: {e}"))?;
            let fimg = fimage::fimg::from_png(&png, pct, render_scale, slice)
                .map_err(|e| format!("{theme}/{piece} at {pct} %: {e}"))?;
            files.push((format!("{theme}/{piece}"), fimg));
        }
    }
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    Ok(fimage::bundle::build(&refs))
}
