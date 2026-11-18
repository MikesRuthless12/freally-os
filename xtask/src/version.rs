// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask version vX.Y.Z[-beta.N|-rc.N]` (closeout step 3): the version,
//! where it's written once: the workspace's `version`, which every crate takes
//! (and with it `fcore_abi::VERSION`: the bootloader's line, the kernel's, the
//! image's header and the shell's `version`). Then Cargo.lock follows it.

use crate::util;

/// `vX.Y.Z`, `vX.Y.Z-beta.N` or `vX.Y.Z-rc.N`, as the ladder spells them;
/// the version without its `v`.
pub fn parse(v: &str) -> Result<&str, String> {
    let bare = v.strip_prefix('v').ok_or_else(|| format!("{v}: a version starts with v (v0.4.0)"))?;
    let (core, pre) = match bare.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (bare, None),
    };
    let number = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && (s == "0" || !s.starts_with('0'));
    let parts: Vec<&str> = core.split('.').collect();
    let pre_ok = pre.is_none_or(|p| ["beta.", "rc."].iter().any(|k| p.strip_prefix(k).is_some_and(number)));
    if parts.len() == 3 && parts.iter().all(|p| number(p)) && pre_ok {
        Ok(bare)
    } else {
        Err(format!("{v}: not a version on the ladder (vX.Y.Z, -beta.N or -rc.N)"))
    }
}

pub fn version_cmd(args: &[String]) -> Result<(), String> {
    let v = args.first().ok_or("cargo xtask version vX.Y.Z")?;
    let bare = parse(v)?;
    let path = util::repo_root().join("Cargo.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    // The one `version = "…"` of [workspace.package].
    let start = text.find("[workspace.package]").ok_or("Cargo.toml has no [workspace.package]")?;
    let at = start + text[start..].find("\nversion = \"").ok_or("[workspace.package] has no version")? + 1;
    let end = at + text[at..].find('\n').ok_or("Cargo.toml ends mid-line")?;
    let was = text[at..end].to_string();
    let now = format!("version = \"{bare}\"");
    std::fs::write(&path, format!("{}{now}{}", &text[..at], &text[end..])).map_err(|e| e.to_string())?;
    // Cargo.lock follows (every workspace crate's version), nothing else moves.
    util::run_ok(util::cargo().args(["update", "--workspace", "--offline", "--quiet"]))?;
    println!("version: {was} → {now} (Cargo.toml's [workspace.package], and Cargo.lock)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn the_ladders_versions_and_nothing_else() {
        assert_eq!(parse("v0.4.0"), Ok("0.4.0"));
        assert_eq!(parse("v1.0.0-rc.2"), Ok("1.0.0-rc.2"));
        assert_eq!(parse("v1.0.0-beta.1"), Ok("1.0.0-beta.1"));
        for bad in ["0.4.0", "v0.4", "v0.4.0.1", "v0.04.0", "v0.4.0-alpha.1", "v0.4.0-rc.", "v0.4.x"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
