// SPDX-License-Identifier: Apache-2.0
//! The playbook's hardware set in the OS (TASK-051, FR-025).
//!
//! - `cargo xtask sync-hardware` copies `Freally/renders/hardware/{dark,light}`
//!   byte for byte into `assets/hardware/` (read-only on the source): the
//!   PNGs (the image never decodes the WebPs, nor any other outside format),
//!   their animation frames, and `hardware.json`. It seals the copy in
//!   `assets/hardware/MANIFEST` (each file's SHA-256).
//! - The **hardware set** gate: the copy is what its MANIFEST says, nothing
//!   more or less; on a machine with the playbook, it is the playbook's set.
//! - `cargo xtask assets` turns each PNG into FIMG at 3x, 2x and 1x
//!   (`target/assets/hardware/<theme>/<name>@<n>x.fimg`). The renders are 4x
//!   (`hardware.json`'s `scale`), so 3x is 3/4 of them, 2x half, 1x a quarter.
//!   The image carries the FIMGs the OS draws (from TASK-056's controls).

use crate::util;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const THEMES: [&str; 2] = ["dark", "light"];
/// The renders' density: `hardware.json` gives each piece at 4x.
const RENDER_SCALE: u32 = 4;

fn source() -> PathBuf {
    util::repo_root().parent().map(|p| p.join("Freally/renders/hardware")).unwrap_or_default()
}

fn copy_dir() -> PathBuf {
    util::repo_root().join("assets/hardware")
}

/// Every file of a theme the OS takes, relative to the theme's folder.
fn wanted(theme_dir: &Path) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for sub in ["", "frames"] {
        let dir = theme_dir.join(sub);
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries {
            let name = e.map_err(|e| e.to_string())?.file_name().to_string_lossy().into_owned();
            if name.ends_with(".png") || (sub.is_empty() && name == "hardware.json") {
                out.push(if sub.is_empty() { name } else { format!("{sub}/{name}") });
            }
        }
    }
    out.sort();
    Ok(out)
}

/// What the copy should hold: `theme/path` → SHA-256.
fn manifest_of(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut m = BTreeMap::new();
    for theme in THEMES {
        for f in wanted(&root.join(theme))? {
            let bytes = std::fs::read(root.join(theme).join(&f)).map_err(|e| format!("{theme}/{f}: {e}"))?;
            m.insert(format!("{theme}/{f}"), util::hex(&fcrypto::sha2::sha256(&bytes)));
        }
    }
    Ok(m)
}

fn manifest_text(m: &BTreeMap<String, String>) -> String {
    m.iter().map(|(path, hash)| format!("{hash}  {path}\n")).collect()
}

pub fn sync_hardware_cmd(_args: &[String]) -> Result<(), String> {
    let src = source();
    let m = manifest_of(&src)?;
    if m.is_empty() {
        return Err(format!("no hardware set at {}", src.display()));
    }
    let dir = copy_dir();
    for theme in THEMES {
        let _ = std::fs::remove_dir_all(dir.join(theme));
    }
    for path in m.keys() {
        let to = dir.join(path);
        std::fs::create_dir_all(to.parent().unwrap_or(&dir)).map_err(|e| e.to_string())?;
        std::fs::copy(src.join(path), &to).map_err(|e| format!("{path}: {e}"))?;
    }
    std::fs::write(dir.join("MANIFEST"), manifest_text(&m)).map_err(|e| e.to_string())?;
    println!("sync-hardware: {} files copied and sealed", m.len());
    Ok(())
}

/// The **hardware set** gate.
pub fn gate() -> Result<String, String> {
    let dir = copy_dir();
    let sealed = std::fs::read_to_string(dir.join("MANIFEST")).map_err(|e| format!("assets/hardware/MANIFEST: {e}"))?;
    let have = manifest_text(&manifest_of(&dir)?);
    if have != sealed {
        return Err(
            "assets/hardware isn't what its MANIFEST says (edited, added or missing): cargo xtask sync-hardware".into(),
        );
    }
    let n = sealed.lines().count();
    let src = source();
    if src.join("dark").exists() {
        if manifest_text(&manifest_of(&src)?) != sealed {
            return Err("assets/hardware isn't the playbook's set: cargo xtask sync-hardware".into());
        }
        return Ok(format!("{n} files, sealed, and the playbook's set"));
    }
    Ok(format!("{n} files, sealed"))
}

/// 9-slice insets at the render's scale, by file (from `hardware.json`).
fn slice_of(name: &str) -> u32 {
    if name == "meter_bezel.png" { 44 } else { 0 }
}

/// `cargo xtask assets`: every PNG of the set as FIMG at 3x, 2x and 1x.
pub fn assets_cmd(_args: &[String]) -> Result<(), String> {
    let start = std::time::Instant::now();
    let (n, bytes) = assets()?;
    println!("assets: {n} FIMGs, {:.1} MB, in {:.1} s", bytes as f64 / 1e6, start.elapsed().as_secs_f64());
    Ok(())
}

pub fn assets() -> Result<(usize, usize), String> {
    let out = util::target_dir().join("assets/hardware");
    let (mut n, mut total) = (0, 0);
    for theme in THEMES {
        for f in wanted(&copy_dir().join(theme))?.iter().filter(|f| f.ends_with(".png")) {
            let png = std::fs::read(copy_dir().join(theme).join(f)).map_err(|e| e.to_string())?;
            let d = fimage::png::decode(&png).map_err(|e| format!("{theme}/{f}: {e}"))?;
            if d.channels != 4 {
                return Err(format!("{theme}/{f}: not RGBA"));
            }
            let premul = fimage::fimg::from_rgba(&d.pixels);
            let stem = f.trim_end_matches(".png");
            let name = Path::new(f).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            for scale in [3u32, 2, 1] {
                let (w, h, px) = fimage::fimg::downscale(d.w, d.h, &premul, scale, RENDER_SCALE);
                let inset = (slice_of(&name) * scale / RENDER_SCALE) as u16;
                let file = fimage::fimg::encode(w, h, scale as u8, [inset; 4], &px);
                let to = out.join(theme).join(format!("{stem}@{scale}x.fimg"));
                std::fs::create_dir_all(to.parent().unwrap_or(&out)).map_err(|e| e.to_string())?;
                std::fs::write(&to, &file).map_err(|e| e.to_string())?;
                n += 1;
                total += file.len();
            }
        }
    }
    Ok((n, total))
}
