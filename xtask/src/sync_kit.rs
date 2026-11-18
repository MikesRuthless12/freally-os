// SPDX-License-Identifier: Apache-2.0
//! The Freally UI kit in the OS (TASK-050).
//!
//! - `cargo xtask sync-kit` copies `Freally Bugs/ui-kit/kit/{theme.css,
//!   manifest.json}` byte for byte into `assets/kit/` (read-only on the
//!   source), then writes `lib/fui/src/tokens_gen.rs` from the copy.
//! - The **ui kit** gate: the copy matches its manifest's hash; on a machine
//!   with the source folder, it is the source byte for byte; and
//!   `tokens_gen.rs` is what the copy generates.
//! - The **contrast** gate: every pair the OS draws meets vision § 5, in both
//!   themes (text 4.5:1, focus rings, meters and control lines 3:1).
//!
//! The generated tokens: every colour (hex, `rgb()`, or a `var()` naming
//! one), every length in px and every plain number, per theme. Shadows,
//! gradients and the like have no single value; they're listed, not lost.

use crate::util;
use std::collections::BTreeMap;
use std::path::PathBuf;

const GEN: &str = "lib/fui/src/tokens_gen.rs";

fn source() -> PathBuf {
    util::repo_root().parent().map(|p| p.join("Freally Bugs/ui-kit/kit")).unwrap_or_default()
}

fn copy_dir() -> PathBuf {
    util::repo_root().join("assets/kit")
}

/// The manifest's hash for theme.css.
fn manifest_hash(manifest: &[u8]) -> Result<String, String> {
    let text = std::str::from_utf8(manifest).map_err(|_| "kit/manifest.json isn't UTF-8")?;
    let at = text.find("\"theme.css\"").ok_or("kit/manifest.json has no theme.css")?;
    let rest = &text[at + "\"theme.css\"".len()..];
    let start = rest.find('"').ok_or("kit/manifest.json: theme.css has no hash")? + 1;
    let end = rest[start..].find('"').ok_or("kit/manifest.json: an unterminated hash")? + start;
    Ok(rest[start..end].to_string())
}

fn sealed(css: &[u8], manifest: &[u8]) -> Result<(), String> {
    let want = manifest_hash(manifest)?;
    let have = util::hex(&fcrypto::sha2::sha256(css));
    if want != have {
        return Err(format!("theme.css is {have}, its manifest says {want}"));
    }
    Ok(())
}

pub fn sync_kit_cmd(_args: &[String]) -> Result<(), String> {
    let src = source();
    let css = std::fs::read(src.join("theme.css")).map_err(|e| format!("{}: {e}", src.display()))?;
    let manifest = std::fs::read(src.join("manifest.json")).map_err(|e| format!("{}: {e}", src.display()))?;
    sealed(&css, &manifest).map_err(|e| format!("the source kit isn't sealed: {e} (re-seal it in Freally Bugs)"))?;
    let dir = copy_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("theme.css"), &css).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("manifest.json"), &manifest).map_err(|e| e.to_string())?;
    let text = std::str::from_utf8(&css).map_err(|_| "theme.css isn't UTF-8")?;
    std::fs::write(util::repo_root().join(GEN), generate(text)?).map_err(|e| e.to_string())?;
    println!("sync-kit: kit {} copied; {GEN} written", manifest_hash(&manifest)?.get(..12).unwrap_or("?"));
    Ok(())
}

/// The **ui kit** gate.
pub fn gate() -> Result<String, String> {
    let dir = copy_dir();
    let css = std::fs::read(dir.join("theme.css")).map_err(|e| format!("assets/kit/theme.css: {e}"))?;
    let manifest = std::fs::read(dir.join("manifest.json")).map_err(|e| format!("assets/kit/manifest.json: {e}"))?;
    sealed(&css, &manifest).map_err(|e| format!("assets/kit was edited: {e} (cargo xtask sync-kit)"))?;
    let src = source();
    let mut said = String::from("the copy is sealed");
    if src.join("manifest.json").exists() {
        let theirs = std::fs::read(src.join("manifest.json")).map_err(|e| e.to_string())?;
        let their_css = std::fs::read(src.join("theme.css")).map_err(|e| e.to_string())?;
        if theirs != manifest || their_css != css {
            return Err("assets/kit isn't Freally Bugs' kit: cargo xtask sync-kit".into());
        }
        said.push_str(", and is Freally Bugs' kit");
    }
    let text = std::str::from_utf8(&css).map_err(|_| "theme.css isn't UTF-8")?;
    let committed = std::fs::read_to_string(util::repo_root().join(GEN)).unwrap_or_default();
    if generate(text)? != committed {
        return Err(format!("{GEN} is stale or edited: cargo xtask sync-kit"));
    }
    Ok(format!("{said}; tokens current"))
}

/// One theme's tokens, by name (without the `--`), as written.
type Block = BTreeMap<String, String>;

fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        rest = rest[i + 2..].find("*/").map_or("", |j| &rest[i + 2 + j + 2..]);
    }
    out.push_str(rest);
    out
}

/// The dark block (`:root, :root[data-theme='dark']`) and the light one.
pub fn themes(css: &str) -> Result<(Block, Block), String> {
    let css = strip_comments(css);
    let block = |selector: &str| -> Result<Block, String> {
        let at = css.find(selector).ok_or(format!("theme.css has no {selector} block"))?;
        let open = css[at..].find('{').ok_or("an unopened block")? + at + 1;
        let close = css[open..].find('}').ok_or("an unclosed block")? + open;
        let mut out = Block::new();
        for decl in css[open..close].split(';') {
            let decl = decl.trim();
            if let Some(d) = decl.strip_prefix("--")
                && let Some((name, value)) = d.split_once(':')
            {
                out.insert(name.trim().to_string(), value.split_whitespace().collect::<Vec<_>>().join(" "));
            }
        }
        Ok(out)
    };
    Ok((block(":root[data-theme='dark'] {")?, block(":root[data-theme='light'] {")?))
}

/// A token's colour, following `var()` within its theme.
pub fn colour(block: &Block, name: &str) -> Option<[u8; 4]> {
    let mut v = block.get(name)?.as_str();
    for _ in 0..8 {
        if let Some(inner) = v.strip_prefix("var(--").and_then(|r| r.strip_suffix(')')) {
            v = block.get(inner)?.as_str();
        } else {
            break;
        }
    }
    if let Some(h) = v.strip_prefix('#')
        && h.len() == 6
    {
        let n = u32::from_str_radix(h, 16).ok()?;
        return Some([(n >> 16) as u8, (n >> 8) as u8, n as u8, 255]);
    }
    let inner = v.strip_prefix("rgb(")?.strip_suffix(')')?;
    let (rgb, alpha) = inner.split_once('/').map_or((inner, None), |(c, a)| (c, Some(a.trim())));
    let c: Vec<u8> = rgb.split_whitespace().map(|x| x.parse().ok()).collect::<Option<_>>()?;
    let a = match alpha {
        None => 255,
        Some(p) => (p.strip_suffix('%')?.parse::<f64>().ok()? * 2.55).round() as u8,
    };
    (c.len() == 3).then(|| [c[0], c[1], c[2], a])
}

fn ident(name: &str) -> String {
    name.replace('-', "_").to_uppercase()
}

/// `lib/fui/src/tokens_gen.rs`, from theme.css.
pub fn generate(css: &str) -> Result<String, String> {
    let (dark, light) = themes(css)?;
    let mut out = String::from(
        "// SPDX-License-Identifier: Apache-2.0\n\
         // @generated by `cargo xtask sync-kit` from assets/kit/theme.css (the Freally UI kit).\n\
         // Never edit: the ui kit gate refuses a copy that isn't what the kit generates.\n\
         //! The UI kit's tokens, per theme (TASK-050).\n\n\
         use crate::color::Rgba;\n",
    );
    for (module, block) in [("dark", &dark), ("light", &light)] {
        let mut colours = Vec::new();
        let mut lengths = Vec::new();
        let mut numbers = Vec::new();
        let mut skipped = Vec::new();
        for (name, value) in block {
            if let Some([r, g, b, a]) = colour(block, name) {
                colours.push(format!("    pub const {}: Rgba = Rgba::new({r}, {g}, {b}, {a});\n", ident(name)));
            } else if let Some(px) = value.strip_suffix("px").and_then(|n| n.parse::<u32>().ok()) {
                lengths.push(format!("    pub const {}: u32 = {px};\n", ident(name)));
            } else if let Ok(n) = value.parse::<f32>() {
                numbers.push(format!("    pub const {}: f32 = {n:?};\n", ident(name)));
            } else {
                skipped.push(name.as_str());
            }
        }
        out.push_str(&format!("\n/// The {module} theme.\npub mod {module} {{\n    use super::Rgba;\n\n"));
        for line in colours.iter().chain(&lengths).chain(&numbers) {
            out.push_str(line);
        }
        out.push_str(&format!(
            "\n    /// No single value (shadows, gradients, calc, images): {}.\n",
            skipped.join(", ")
        ));
        out.push_str("    pub const COMPOSITE: &[&str] = &[\n");
        for s in &skipped {
            out.push_str(&format!("        \"{s}\",\n"));
        }
        out.push_str("    ];\n}\n");
    }
    Ok(out)
}

/// WCAG relative luminance of an opaque colour.
fn luminance([r, g, b, _]: [u8; 4]) -> f64 {
    let lin = |c: u8| {
        let c = c as f64 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

/// `fg` composited over the opaque `bg`, then the WCAG ratio between them.
pub fn ratio(fg: [u8; 4], bg: [u8; 4]) -> f64 {
    let a = fg[3] as f64 / 255.0;
    let mix = |f: u8, b: u8| (f as f64 * a + b as f64 * (1.0 - a)).round() as u8;
    let over = [mix(fg[0], bg[0]), mix(fg[1], bg[1]), mix(fg[2], bg[2]), 255];
    let (l1, l2) = (luminance(over), luminance(bg));
    (l1.max(l2) + 0.05) / (l1.min(l2) + 0.05)
}

/// The pairs the OS draws, and the ratio each needs (vision § 5).
pub const PAIRS: &[(&[&str], &[&str], f64)] = &[
    // Words on every ground and surface.
    (
        &[
            "color-text",
            "color-text-2",
            "color-text-3",
            "color-primary-ink",
            "color-success",
            "color-warning",
            "color-error",
        ],
        &["color-bg", "color-surface", "color-surface-2"],
        4.5,
    ),
    // Words on a green fill.
    (&["color-on-primary"], &["color-primary"], 4.5),
    // Focus rings on every ground and surface.
    (&["color-focus"], &["color-bg", "color-surface", "color-surface-2"], 3.0),
    // A lit LED against an unlit one.
    (&["color-meter-green", "color-meter-yellow", "color-meter-red"], &["color-meter-off"], 3.0),
];

/// The **contrast** gate, on assets/kit/theme.css.
pub fn contrast_gate() -> Result<String, String> {
    let css =
        std::fs::read_to_string(copy_dir().join("theme.css")).map_err(|e| format!("assets/kit/theme.css: {e}"))?;
    check_contrast(&css)
}

pub fn check_contrast(css: &str) -> Result<String, String> {
    let (dark, light) = themes(css)?;
    let mut checked = 0;
    let mut bad = Vec::new();
    for (theme, block) in [("dark", &dark), ("light", &light)] {
        for (fgs, bgs, need) in PAIRS {
            for fg in *fgs {
                for bg in *bgs {
                    let f = colour(block, fg).ok_or(format!("{theme}: --{fg} isn't a colour"))?;
                    let b = colour(block, bg).ok_or(format!("{theme}: --{bg} isn't a colour"))?;
                    let r = ratio(f, b);
                    checked += 1;
                    if r < *need {
                        bad.push(format!("{theme}: --{fg} on --{bg} is {r:.2}:1, under {need}:1"));
                    }
                }
            }
        }
    }
    if bad.is_empty() { Ok(format!("{checked} pairs in both themes meet vision § 5")) } else { Err(bad.join("; ")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wcag_ratio_is_right() {
        assert!((ratio([0, 0, 0, 255], [255, 255, 255, 255]) - 21.0).abs() < 0.01);
        assert!((ratio([255, 255, 255, 255], [255, 255, 255, 255]) - 1.0).abs() < 0.01);
        // #767676 on white is the classic 4.54:1.
        assert!((ratio([0x76, 0x76, 0x76, 255], [255, 255, 255, 255]) - 4.54).abs() < 0.01);
    }

    #[test]
    fn colours_parse_and_follow_var() {
        let css = ":root[data-theme='dark'] { --a: #79d920; --b: rgb(255 255 255 / 5%); --c: var(--a); --d: 44px; }\n\
                   :root[data-theme='light'] { --a: #000000; }";
        let (dark, _) = themes(css).unwrap();
        assert_eq!(colour(&dark, "a"), Some([0x79, 0xd9, 0x20, 255]));
        assert_eq!(colour(&dark, "b"), Some([255, 255, 255, 13]));
        assert_eq!(colour(&dark, "c"), Some([0x79, 0xd9, 0x20, 255]));
        assert_eq!(colour(&dark, "d"), None);
    }

    #[test]
    fn a_low_contrast_pair_is_refused() {
        let mut lines = vec![":root[data-theme='dark'] {".to_string()];
        for (name, v) in [
            ("color-bg", "#000000"),
            ("color-surface", "#000000"),
            ("color-surface-2", "#000000"),
            ("color-text", "#ffffff"),
            ("color-text-2", "#ffffff"),
            ("color-text-3", "#333333"), // 1.66:1 on black
            ("color-primary-ink", "#ffffff"),
            ("color-success", "#ffffff"),
            ("color-warning", "#ffffff"),
            ("color-error", "#ffffff"),
            ("color-on-primary", "#000000"),
            ("color-primary", "#ffffff"),
            ("color-focus", "#ffffff"),
            ("color-meter-green", "#ffffff"),
            ("color-meter-yellow", "#ffffff"),
            ("color-meter-red", "#ffffff"),
            ("color-meter-off", "#000000"),
        ] {
            lines.push(format!("--{name}: {v};"));
        }
        lines.push("}".into());
        let block = lines.join("\n");
        let css = format!("{block}\n{}", block.replace(":root[data-theme='dark']", ":root[data-theme='light']"));
        let err = check_contrast(&css).unwrap_err();
        assert!(err.contains("--color-text-3 on --color-bg"), "{err}");
    }
}
