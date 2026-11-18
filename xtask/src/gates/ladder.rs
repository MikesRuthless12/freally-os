// SPDX-License-Identifier: Apache-2.0
//! The ladder gate: the docs site's `site/ladder.json` must list exactly the
//! roadmap's version ladder (rung and phase, in order). The roadmap is private,
//! so where it isn't present (a fresh clone, the ci-linux container) the gate
//! says SKIP with why; so does it before the site exists (Phase 1).

use crate::util;
use fjson::Value;

/// `(version, phase)` rows from the roadmap's ladder table.
pub fn roadmap_rungs(roadmap: &str) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    let mut inside = false;
    for line in roadmap.lines() {
        if line.starts_with("## ⛔ THE VERSION LADDER") {
            inside = true;
            continue;
        }
        if inside && line.starts_with("## ") {
            break;
        }
        if !inside || !line.starts_with("| ") {
            continue;
        }
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        let (Some(v), Some(p)) = (cells.get(1), cells.get(2)) else { continue };
        let v = v.trim_matches('*').trim_matches('`');
        if !v.starts_with('v') {
            continue;
        }
        let phase = p.split(" — ").next().unwrap_or("").trim().to_string();
        rows.push((v.to_string(), phase));
    }
    rows
}

pub fn check() -> Result<String, String> {
    let root = util::repo_root();
    let site = root.join("site/ladder.json");
    if !site.is_file() {
        return Ok("SKIP: no site/ladder.json yet (the docs site arrives in Phase 1)".into());
    }
    let Ok(roadmap) = std::fs::read_to_string(root.join("docs/product-roadmap.md")) else {
        return Ok("SKIP: the roadmap is private and not in this checkout".into());
    };
    let want = roadmap_rungs(&roadmap);
    let text = std::fs::read_to_string(&site).map_err(|e| e.to_string())?;
    let v = fjson::parse(&text).map_err(|e| format!("site/ladder.json at byte {}: {}", e.at, e.what))?;
    let got: Vec<(String, String)> = v
        .get("rungs")
        .and_then(Value::as_array)
        .ok_or("site/ladder.json has no rungs")?
        .iter()
        .map(|r| {
            (
                r.get("version").and_then(Value::as_str).unwrap_or("").to_string(),
                r.get("phase").and_then(Value::as_str).unwrap_or("").to_string(),
            )
        })
        .collect();
    if got == want {
        Ok(format!("{} rungs match the roadmap", want.len()))
    } else {
        let first = want.iter().zip(&got).position(|(a, b)| a != b).unwrap_or(want.len().min(got.len()));
        Err(format!(
            "site/ladder.json differs from the roadmap at rung {}: roadmap {:?}, site {:?}",
            first + 1,
            want.get(first),
            got.get(first)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::roadmap_rungs;

    #[test]
    fn reads_the_ladder_table() {
        let md = "## ⛔ THE VERSION LADDER — one ladder\n\n| Version | Phase | What | MVP? |\n|---|---|---|---|\n| **`v0.1.0`** | 0 — Foundation | x | ✅ |\n| `v0.30.5` | 29A — The macOS layer | | |\n| `v1.1.0` … `v2.0.0` | Part III | | |\n\n## Next\n| `v9.9.9` | 9 — no | | |\n";
        assert_eq!(
            roadmap_rungs(md),
            vec![
                ("v0.1.0".to_string(), "0".to_string()),
                ("v0.30.5".to_string(), "29A".to_string()),
                ("v1.1.0` … `v2.0.0".to_string(), "Part III".to_string())
            ]
        );
    }
}
