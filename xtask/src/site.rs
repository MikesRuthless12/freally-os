// SPDX-License-Identifier: Apache-2.0
//! The docs site (TASK-032): `site/`, its own private repository, built from the
//! fleet site kit (`site/kit/`, never edited here). `cargo xtask site-build`
//! runs the kit's builder, then its check; the `site` gate runs the check only
//! (drift, a missing translation, a broken link, an inline script or a
//! third-party URL fails it). Where the site isn't checked out (a fresh clone,
//! the ci-linux container) the gate says SKIP with why.

use crate::util;
use std::process::Command;

fn node(args: &[&str]) -> Result<String, String> {
    let site = util::repo_root().join("site");
    let out = Command::new("node")
        .current_dir(&site)
        .args(args)
        .output()
        .map_err(|e| format!("couldn't start node: {e} (the site needs Node.js)"))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if out.status.success() {
        Ok(text.lines().last().unwrap_or("").to_string())
    } else {
        let lines: Vec<&str> = text.lines().collect();
        Err(lines[lines.len().saturating_sub(20)..].join("\n"))
    }
}

pub fn check() -> Result<String, String> {
    let site = util::repo_root().join("site");
    if !site.join("site.json").is_file() {
        return Ok("SKIP: the docs site (its own private repository) isn't checked out here".into());
    }
    node(&["kit/build.mjs", "--check"])
}

pub fn build_cmd(_args: &[String]) -> Result<(), String> {
    if !util::repo_root().join("site/site.json").is_file() {
        return Err("no site/site.json: clone the site's repository into site/".into());
    }
    println!("{}", node(&["kit/build.mjs"])?);
    println!("{}", node(&["kit/build.mjs", "--check"])?);
    Ok(())
}
