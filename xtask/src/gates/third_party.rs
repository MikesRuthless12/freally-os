// SPDX-License-Identifier: Apache-2.0
//! The no-third-party gate: `Cargo.lock` may list only this workspace's crates.
//!
//! A package from crates.io, a git repository or any other registry carries a
//! `source = "…"` line in `Cargo.lock`; a workspace crate never does. Charter
//! rule 1: every line is ours (Rust's own core/alloc/compiler_builtins are part
//! of the toolchain and never appear in `Cargo.lock`).

use crate::util;

pub fn check() -> Result<String, String> {
    // Make sure Cargo.lock reflects the manifests as they are now.
    util::run_ok(
        util::cargo().args(["metadata", "--format-version", "1", "--quiet"]).stdout(std::process::Stdio::null()),
    )?;
    let lock = std::fs::read_to_string(util::repo_root().join("Cargo.lock"))
        .map_err(|e| format!("can't read Cargo.lock: {e}"))?;
    let outside = outside_packages(&lock);
    if outside.is_empty() {
        let n = lock.matches("[[package]]").count();
        Ok(format!("{n} packages, all in this workspace"))
    } else {
        Err(format!("packages from outside this workspace: {}", outside.join(", ")))
    }
}

/// `name version (source)` for every package in `lock` that has a source.
pub fn outside_packages(lock: &str) -> Vec<String> {
    let mut found = Vec::new();
    for block in lock.split("[[package]]").skip(1) {
        let field = |key: &str| {
            block.lines().find_map(|l| {
                let l = l.trim();
                l.strip_prefix(key)
                    .and_then(|rest| rest.trim_start().strip_prefix('='))
                    .map(|v| v.trim().trim_matches('"').to_string())
            })
        };
        if let Some(source) = field("source") {
            let name = field("name").unwrap_or_else(|| "?".into());
            let version = field("version").unwrap_or_default();
            found.push(format!("{name} {version} ({source})"));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_packages_pass_and_registry_packages_are_named() {
        let lock = "version = 4\n\n[[package]]\nname = \"fcrypto\"\nversion = \"0.1.0\"\n\n\
                    [[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
        let found = outside_packages(lock);
        assert_eq!(found.len(), 1);
        assert!(found[0].starts_with("serde 1.0.0"));
    }
}
