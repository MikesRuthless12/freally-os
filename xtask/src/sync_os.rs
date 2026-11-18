// SPDX-License-Identifier: Apache-2.0
//! The OS's own renders (TASK-052, FR-025): the playbook's
//! `Freally/renders/os/{dark,light}` (from `Freally/blender/os.py`) in the OS.
//!
//! - `cargo xtask sync-os` copies each theme's PNGs and `os.json` byte for
//!   byte into `assets/os/` (read-only on the source) and seals the copy in
//!   `assets/os/MANIFEST`.
//! - The **os set** gate: the copy is what its MANIFEST says; on a machine
//!   with the playbook, it is the playbook's.
//! - `os_bundle(scale)`: every piece of both themes as FIMG at `scale` (each
//!   piece's render scale and 9-slice insets from `os.json`), the wallpaper
//!   at 1x only (a larger screen scales it up: a soft wall bears it).

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

/// Both themes' pieces as FIMGs at `scale`, in one bundle: `dark/<piece>`,
/// `light/<piece>` (the wallpaper only at 1x).
pub fn os_bundle(scale: u32) -> Result<Vec<u8>, String> {
    let dir = copy_dir();
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for theme in THEMES {
        for (piece, render_scale, slice) in pieces()? {
            if piece == "wallpaper" && scale != 1 {
                continue;
            }
            let png = std::fs::read(dir.join(theme).join(format!("{piece}.png")))
                .map_err(|e| format!("assets/os/{theme}/{piece}.png: {e}"))?;
            let fimg = fimage::fimg::from_png(&png, scale, render_scale, slice)
                .map_err(|e| format!("{theme}/{piece}: {e}"))?;
            files.push((format!("{theme}/{piece}"), fimg));
        }
    }
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    Ok(fimage::bundle::build(&refs))
}
