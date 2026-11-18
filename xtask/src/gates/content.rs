// SPDX-License-Identifier: Apache-2.0
//! The gates that read the tree's contents:
//!
//! - **no-AI** (charter rule 2): no model or inference crate, no model file;
//! - **denylist**: no other company's product names in UI strings or the site,
//!   and the fleet's three scrubbed names nowhere at all (matched by hash, so the
//!   names themselves are never written here);
//! - **SPDX**: every source file starts with `SPDX-License-Identifier: Apache-2.0`;
//! - **private-docs** (Freally Handscore's rules, ported): nothing private is even
//!   trackable in this public repository, and the site holds no private doc or
//!   its fingerprints.

use crate::util;
use std::path::{Path, PathBuf};

/// Crate names that mean a model or inference runtime.
const AI_CRATES: &[&str] = &[
    "tch",
    "torch",
    "torch-sys",
    "ort",
    "onnxruntime",
    "onnx",
    "tract-onnx",
    "candle-core",
    "candle-nn",
    "llama-cpp",
    "llama_cpp",
    "llm",
    "ggml",
    "whisper-rs",
    "rust-bert",
    "tokenizers",
    "safetensors",
    "hf-hub",
    "tensorflow",
    "burn",
    "async-openai",
    "openai",
    "anthropic",
    "ollama-rs",
    "mistralrs",
    "kalosm",
];
/// File extensions that mean model weights.
const AI_EXTENSIONS: &[&str] =
    &["onnx", "gguf", "ggml", "safetensors", "pt", "pth", "ckpt", "tflite", "mlmodel", "h5", "pb"];

/// Product names that never appear in Freally OS's UI or site (THE APP REGISTER).
const PRODUCT_NAMES: &[&str] = &[
    "PowerPoint",
    "Excel",
    "Microsoft Word",
    "MS Word",
    "OneNote",
    "Outlook",
    "Keynote",
    "Google Docs",
    "Google Sheets",
    "Google Slides",
    "Photoshop",
    "Notepad++",
    "iWork",
    "Microsoft 365",
    "Office 365",
];
/// SHA-256 of the fleet's three scrubbed names, lowercase (never written out).
const SCRUBBED: &[&str] = &[
    "4d9de43a230a3820007d0f2c0faa5559f7d259be2479b6c4900738dd6c127906",
    "0c3b42b9709411c48b2ef80d8c3f499caaafba1fba6eb654c15aa8d17d74e458",
    "1da0672bd372f181d79fd5391aaeda10d256ebfd95ddb09ab9b51e52affbdbfc",
];
/// The one allowed use: the instrument "Starlight Piano" / `PianoStarlight`.
const SCRUBBED_EXCEPTION_NEXT: &str = "piano";

/// Names a public file may never have (the house `.gitignore`'s private set).
const PRIVATE_PATTERNS: &[&str] = &[
    "roadmap",
    "build-prompts",
    "prd.md",
    "product-vision",
    "gtm.md",
    "vision.json",
    "live-to-do",
    // The handoff note (HANDOFF.md, HANDOFF-*.md), never the bootloader's
    // `handoff.rs`.
    "handoff.md",
    "handoff-",
    "claude.md",
    "docs/research/",
    "private/",
];

/// A path one of the private documents would have (any case).
fn is_private_path(path: &str) -> bool {
    let s = path.to_lowercase();
    PRIVATE_PATTERNS.iter().any(|p| s.contains(p))
}

fn tracked() -> Result<Vec<PathBuf>, String> {
    util::git_files()
}

fn is_text(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).unwrap_or(""),
        "rs" | "toml" | "md" | "json" | "ps1" | "sh" | "mjs" | "js" | "html" | "css" | "txt" | "ron" | "ld" | "yml"
    ) || p.file_name().is_some_and(|n| n == "Dockerfile" || n.to_string_lossy().ends_with(".Dockerfile"))
}

pub fn no_ai() -> Result<String, String> {
    let root = util::repo_root();
    let mut found = Vec::new();
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).unwrap_or_default();
    for line in lock.lines() {
        if let Some(name) = line.trim().strip_prefix("name = ") {
            let name = name.trim_matches('"');
            if AI_CRATES.contains(&name) {
                found.push(format!("crate {name}"));
            }
        }
    }
    let files = tracked()?;
    for f in &files {
        if let Some(ext) = f.extension().and_then(|e| e.to_str())
            && AI_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
        {
            found.push(format!("model file {}", f.display()));
        }
    }
    if found.is_empty() {
        Ok(format!("{} files, no model or inference code", files.len()))
    } else {
        Err(found.join(", "))
    }
}

/// Lower-case word tokens of `text`, letters only.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphabetic()).filter(|w| !w.is_empty()).map(|w| w.to_lowercase()).collect()
}

fn scrubbed_in(text: &str) -> bool {
    let w = words(text);
    w.iter().enumerate().any(|(i, word)| {
        let h = util::hex(&fcrypto::sha2::sha256(word.as_bytes()));
        SCRUBBED.contains(&h.as_str()) && w.get(i + 1).map(String::as_str) != Some(SCRUBBED_EXCEPTION_NEXT)
    })
}

pub fn denylist() -> Result<String, String> {
    let root = util::repo_root();
    let mut found = Vec::new();
    let files = tracked()?;
    // The scrubbed names: nowhere in the tree (text files), not even in comments.
    for f in files.iter().filter(|f| is_text(f)) {
        let text = std::fs::read_to_string(root.join(f)).unwrap_or_default();
        if scrubbed_in(&text) {
            found.push(format!("a scrubbed name in {}", f.display()));
        }
    }
    // Product names: in what the user reads (screens, string catalogs, the site).
    // The site is its own repository (ignored here), so it's read from disk.
    let mut ui: Vec<PathBuf> = files
        .iter()
        .filter(|f| {
            let s = f.to_string_lossy();
            s.starts_with("lib/fscreen/") || s.starts_with("assets/strings/")
        })
        .cloned()
        .collect();
    let mut site = Vec::new();
    util::walk(&root.join("site"), &mut site);
    for f in site.into_iter().filter(|f| is_text(f)) {
        let text = std::fs::read_to_string(&f).unwrap_or_default();
        if scrubbed_in(&text) {
            found.push(format!("a scrubbed name in {}", f.display()));
        }
        ui.push(f.strip_prefix(&root).map(PathBuf::from).unwrap_or(f));
    }
    for f in &ui {
        let text = std::fs::read_to_string(root.join(f)).unwrap_or_default();
        for name in PRODUCT_NAMES {
            if text.contains(name) {
                found.push(format!("{name:?} in {}", f.display()));
            }
        }
    }
    if found.is_empty() {
        Ok(format!("{} files clean, {} of them user-facing", files.len(), ui.len()))
    } else {
        Err(found.join(", "))
    }
}

pub fn spdx() -> Result<String, String> {
    let root = util::repo_root();
    let mut missing = Vec::new();
    let mut n = 0;
    for f in tracked()? {
        let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("");
        let source = matches!(ext, "rs" | "toml" | "ps1" | "sh" | "mjs" | "js" | "ld" | "ron")
            || f.file_name().is_some_and(|n| n.to_string_lossy().ends_with("Dockerfile"));
        if !source {
            continue;
        }
        n += 1;
        let text = std::fs::read_to_string(root.join(&f)).unwrap_or_default();
        if !text.lines().take(5).any(|l| l.contains("SPDX-License-Identifier: Apache-2.0")) {
            missing.push(f.display().to_string());
        }
    }
    if missing.is_empty() {
        Ok(format!("{n} source files"))
    } else {
        Err(format!("no SPDX line: {}", missing.join(", ")))
    }
}

pub fn private_docs() -> Result<String, String> {
    let mut found = Vec::new();
    let files = tracked()?;
    for f in &files {
        if is_private_path(&f.to_string_lossy()) {
            found.push(format!("{} is trackable", f.display()));
        }
    }
    let site = util::repo_root().join("site");
    let mut site_files = 0;
    if site.is_dir() {
        let mut all = Vec::new();
        util::walk(&site, &mut all);
        let markers: &[(&str, &str)] =
            &[("TASK-", "a task id"), ("PLAID", "the planning tool"), ("Open Question", "an Open Question")];
        for f in all {
            let rel = f.strip_prefix(util::repo_root()).unwrap_or(&f).display().to_string();
            let name = f.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            if rel.contains("/.git/") {
                continue;
            }
            site_files += 1;
            if name.ends_with(".md") || PRIVATE_PATTERNS.iter().any(|p| name.contains(p)) {
                found.push(format!("{rel} is a private doc or Markdown"));
                continue;
            }
            if is_text(&f) {
                let text = std::fs::read_to_string(&f).unwrap_or_default();
                for (m, what) in markers {
                    if text.contains(m) {
                        found.push(format!("{rel} carries {what}"));
                    }
                }
                let lower = text.to_lowercase();
                for p in [
                    "product-roadmap",
                    "product-vision",
                    "build-prompts",
                    "live-to-do",
                    "prd.md",
                    "gtm.md",
                    "vision.json",
                ] {
                    if lower.contains(p) {
                        found.push(format!("{rel} names {p}"));
                    }
                }
            }
        }
    }
    if found.is_empty() {
        Ok(format!(
            "{} trackable files public; site: {}",
            files.len(),
            if site.is_dir() { format!("{site_files} files clean") } else { "none yet".into() }
        ))
    } else {
        Err(found.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_private_documents_are_caught_and_source_that_shares_a_word_is_not() {
        for private in ["HANDOFF.md", "docs/product-roadmap.md", "Live-To-Do.md", "handoff-2026.md", "CLAUDE.md"] {
            assert!(is_private_path(private), "{private}");
        }
        // The bootloader's handoff to the kernel (it was kept out of the repo).
        assert!(!is_private_path("boot/src/handoff.rs"));
    }

    #[test]
    fn the_scrubbed_names_are_caught_by_hash_and_the_instrument_is_allowed() {
        // Build the words at run time, so this file never holds them either.
        let star = ["star", "light"].concat();
        assert!(scrubbed_in(&format!("a {star} pad")));
        assert!(scrubbed_in(&format!("A {} PAD", star.to_uppercase())));
        assert!(!scrubbed_in(&format!("the {star} piano")));
        assert!(!scrubbed_in("PianoStarlight is one token"));
        assert!(!scrubbed_in("nothing here"));
    }
}
