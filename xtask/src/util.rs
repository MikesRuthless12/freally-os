// SPDX-License-Identifier: Apache-2.0
//! Paths and process helpers. Paths are passed as arguments, never through a
//! shell string: the repository's path has spaces.

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

/// The repository root (the directory above `xtask/`).
pub fn repo_root() -> PathBuf {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.parent().map(Path::to_path_buf).unwrap_or(here)
}

pub fn home() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/root"))
}

/// The build cache, on WSL's own disk: `$CARGO_TARGET_DIR`, else `~/.freally-target`.
pub fn target_dir() -> PathBuf {
    env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| home().join(".freally-target"))
}

/// Where the dev and staging keys live: `~/.freally-keys/`.
pub fn keys_dir() -> PathBuf {
    home().join(".freally-keys")
}

/// Where VM folders live: `~/.freally-vms/<name>/`.
pub fn vms_dir() -> PathBuf {
    home().join(".freally-vms")
}

/// A `cargo` command with the target dir set and the repo as its working directory.
pub fn cargo() -> Command {
    let mut c = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    c.current_dir(repo_root());
    c.env("CARGO_TARGET_DIR", target_dir());
    c
}

/// Runs `cmd`, inheriting stdio. `Err` names the command if it couldn't start.
pub fn run(cmd: &mut Command) -> Result<ExitStatus, String> {
    cmd.status().map_err(|e| format!("couldn't start {:?}: {e}", cmd.get_program()))
}

/// Runs `cmd` and requires success.
pub fn run_ok(cmd: &mut Command) -> Result<(), String> {
    let status = run(cmd)?;
    if status.success() { Ok(()) } else { Err(format!("{:?} failed ({status})", cmd.get_program())) }
}

/// Runs `cmd` and captures stdout (stderr inherited).
pub fn capture(cmd: &mut Command) -> Result<String, String> {
    let out =
        cmd.stderr(Stdio::inherit()).output().map_err(|e| format!("couldn't start {:?}: {e}", cmd.get_program()))?;
    if !out.status.success() {
        return Err(format!("{:?} failed ({})", cmd.get_program(), out.status));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `true` if `prog` is on `PATH`.
pub fn on_path(prog: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path).map(|d| d.join(prog)).find(|p| p.is_file())
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub fn unhex(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return Err("odd number of hex digits".into());
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| format!("bad hex: {e}"))).collect()
}

/// Every file under `dir` (recursively), skipping `target/`, `.git/` and `node_modules/`.
pub fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if p.is_dir() {
            if matches!(name.as_ref(), "target" | ".git" | "node_modules") {
                continue;
            }
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// Files git tracks or would track (tracked + untracked-not-ignored), relative to the repo.
pub fn git_files() -> Result<Vec<PathBuf>, String> {
    let out = capture(Command::new("git").current_dir(repo_root()).args([
        "ls-files",
        "--cached",
        "--others",
        "--exclude-standard",
        "-z",
    ]))?;
    Ok(out.split('\0').filter(|s| !s.is_empty()).map(PathBuf::from).collect())
}

/// Every file under `dir`, recursively, skipping nothing.
pub fn walk_all(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk_all(&p, out);
        } else {
            out.push(p);
        }
    }
}
