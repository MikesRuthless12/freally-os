// SPDX-License-Identifier: Apache-2.0
//! fsvc: the service manifest (`.svc`) parser (TASK-037, PRD § 3).
//!
//! UTF-8, one `key = value` per line, `#` starts a comment (at the start of a
//! line, or after a space). Lists are comma-separated. Strict: an unknown key,
//! a key given twice, an unknown grant or flavour, a missing or badly written
//! description: each is an error naming the line. `check_set` checks a whole
//! image's manifests: unique names, every `after` known, no cycles.
#![no_std]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Start {
    Boot,
    Demand,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Restart {
    Always,
    OnFailure,
    Never,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    Realtime,
    Interactive,
    Normal,
    Background,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flavour {
    Release,
    Dev,
    E2e,
}

/// The named capability bundles conductor hands over.
pub const GRANTS: &[&str] = &[
    // Kernel grants.
    "process-create",
    "realtime",
    "dma",
    "faults",
    "boot-control",
    "kernel-log",
    // Hardware: the root resource (devmgr), and what devmgr carves for drivers.
    "devices",
    "framebuffer",
    "input",
    "serial",
    // Data: the image's fonts, each a read-only VMO; the hardware set's
    // bundles (one per UI scale); the OS's own renders (TASK-052).
    "fonts",
    "hardware",
    "os",
];

/// Services that may never be in a release image (FR-037).
pub const DEV_ONLY: &[&str] = &["brain", "cdpd", "drv-uart16550"];

pub const MAX_DESCRIPTION: usize = 120;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Manifest {
    pub name: String,
    pub binary: String,
    pub description: String,
    pub start: Start,
    pub after: Vec<String>,
    pub restart: Restart,
    pub class: Class,
    pub grants: Vec<String>,
    pub flavours: Vec<Flavour>,
    pub handoff: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ManifestError {
    /// 1-based; 0 when the problem is the file as a whole.
    pub line: usize,
    pub what: String,
}

impl core::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.line == 0 { write!(f, "{}", self.what) } else { write!(f, "line {}: {}", self.line, self.what) }
    }
}

fn err(line: usize, what: impl Into<String>) -> ManifestError {
    ManifestError { line, what: what.into() }
}

fn valid_name(s: &str) -> bool {
    (1..=32).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn list(v: &str) -> Vec<String> {
    v.split(',').map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect()
}

/// Removes a comment: `#` at the start, or after whitespace.
fn strip_comment(line: &str) -> &str {
    let b = line.as_bytes();
    for i in 0..b.len() {
        if b[i] == b'#' && (i == 0 || b[i - 1].is_ascii_whitespace()) {
            return &line[..i];
        }
    }
    line
}

/// Parses one manifest.
pub fn parse(text: &str) -> Result<Manifest, ManifestError> {
    let mut fields: BTreeMap<&str, (usize, &str)> = BTreeMap::new();
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        let (k, v) = line.split_once('=').ok_or_else(|| err(n, "not a `key = value` line"))?;
        let (k, v) = (k.trim(), v.trim());
        if !matches!(
            k,
            "name"
                | "binary"
                | "description"
                | "start"
                | "after"
                | "restart"
                | "class"
                | "grants"
                | "flavours"
                | "handoff"
        ) {
            return Err(err(n, format!("unknown key {k:?}")));
        }
        if fields.insert(k, (n, v)).is_some() {
            return Err(err(n, format!("{k} is given twice")));
        }
    }
    let get = |k: &str| fields.get(k).copied();
    let need = |k: &str| get(k).ok_or_else(|| err(0, format!("no {k}")));

    let (ln, name) = need("name")?;
    if !valid_name(name) {
        return Err(err(ln, format!("the name {name:?} isn't 1–32 of a–z, 0–9 and -")));
    }
    let (lb, binary) = need("binary")?;
    if !binary.starts_with("/system/") || binary.contains("..") {
        return Err(err(lb, format!("the binary {binary:?} isn't under /system/")));
    }
    let (ld, description) = get("description").ok_or_else(|| err(0, "no description"))?;
    if description.is_empty() {
        return Err(err(ld, "the description is empty"));
    }
    if description.chars().count() > MAX_DESCRIPTION {
        return Err(err(
            ld,
            format!("the description is {} characters (at most {MAX_DESCRIPTION})", description.chars().count()),
        ));
    }
    if !description.ends_with('.') {
        return Err(err(ld, "the description doesn't end with a full stop"));
    }
    let start = match get("start") {
        None | Some((_, "boot")) => Start::Boot,
        Some((_, "demand")) => Start::Demand,
        Some((l, v)) => return Err(err(l, format!("start is boot or demand, not {v:?}"))),
    };
    let restart = match get("restart") {
        None | Some((_, "always")) => Restart::Always,
        Some((_, "on-failure")) => Restart::OnFailure,
        Some((_, "never")) => Restart::Never,
        Some((l, v)) => return Err(err(l, format!("restart is always, on-failure or never, not {v:?}"))),
    };
    let class = match get("class") {
        None | Some((_, "normal")) => Class::Normal,
        Some((_, "realtime")) => Class::Realtime,
        Some((_, "interactive")) => Class::Interactive,
        Some((_, "background")) => Class::Background,
        Some((l, v)) => return Err(err(l, format!("class is realtime, interactive, normal or background, not {v:?}"))),
    };
    let after = match get("after") {
        None => Vec::new(),
        Some((l, v)) => {
            let a = list(v);
            if let Some(bad) = a.iter().find(|x| !valid_name(x)) {
                return Err(err(l, format!("after names {bad:?}, which isn't a service name")));
            }
            a
        }
    };
    let grants = match get("grants") {
        None => Vec::new(),
        Some((l, v)) => {
            let g = list(v);
            if let Some(bad) = g.iter().find(|x| !GRANTS.contains(&x.as_str())) {
                return Err(err(l, format!("unknown grant {bad:?}")));
            }
            g
        }
    };
    let flavours = match get("flavours") {
        None => alloc::vec![Flavour::Release, Flavour::Dev, Flavour::E2e],
        Some((l, v)) => {
            let mut out = Vec::new();
            for f in list(v) {
                out.push(match f.as_str() {
                    "release" => Flavour::Release,
                    "dev" => Flavour::Dev,
                    "e2e" => Flavour::E2e,
                    _ => return Err(err(l, format!("unknown flavour {f:?}"))),
                });
            }
            if out.is_empty() {
                return Err(err(l, "flavours is empty"));
            }
            if out.contains(&Flavour::Release) && DEV_ONLY.contains(&name) {
                return Err(err(l, format!("{name} may never be in a release image")));
            }
            out
        }
    };
    let handoff = match get("handoff") {
        None | Some((_, "false")) => false,
        Some((_, "true")) => true,
        Some((l, v)) => return Err(err(l, format!("handoff is true or false, not {v:?}"))),
    };
    Ok(Manifest {
        name: name.to_string(),
        binary: binary.to_string(),
        description: description.to_string(),
        start,
        after,
        restart,
        class,
        grants,
        flavours,
        handoff,
    })
}

/// Checks an image's manifests together: unique names and binaries, every
/// `after` names a service in the set, and no dependency cycle. Returns the
/// names in a start order (each after everything it waits for).
pub fn check_set(set: &[Manifest]) -> Result<Vec<String>, String> {
    let mut by_name: BTreeMap<&str, &Manifest> = BTreeMap::new();
    for m in set {
        if by_name.insert(&m.name, m).is_some() {
            return Err(format!("two manifests are named {}", m.name));
        }
    }
    let mut binaries = BTreeMap::new();
    for m in set {
        if let Some(other) = binaries.insert(m.binary.as_str(), m.name.as_str()) {
            return Err(format!("{} and {other} share the binary {}", m.name, m.binary));
        }
        for a in &m.after {
            if !by_name.contains_key(a.as_str()) {
                return Err(format!("{} waits for {a}, which no manifest names", m.name));
            }
        }
    }
    // Depth-first, with the path kept so a cycle is reported as one.
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        New,
        Open,
        Done,
    }
    let mut mark: BTreeMap<&str, Mark> = by_name.keys().map(|k| (*k, Mark::New)).collect();
    let mut order = Vec::new();
    fn visit<'a>(
        n: &'a str,
        by_name: &BTreeMap<&'a str, &'a Manifest>,
        mark: &mut BTreeMap<&'a str, Mark>,
        path: &mut Vec<&'a str>,
        order: &mut Vec<String>,
    ) -> Result<(), String> {
        match mark[n] {
            Mark::Done => return Ok(()),
            Mark::Open => {
                let from = path.iter().position(|p| *p == n).unwrap_or(0);
                let mut cycle: Vec<&str> = path[from..].to_vec();
                cycle.push(n);
                return Err(format!("a dependency cycle: {}", cycle.join(" → ")));
            }
            Mark::New => {}
        }
        mark.insert(n, Mark::Open);
        path.push(n);
        for a in &by_name[n].after {
            visit(a, by_name, mark, path, order)?;
        }
        path.pop();
        mark.insert(n, Mark::Done);
        order.push(n.to_string());
        Ok(())
    }
    for n in by_name.keys().copied().collect::<Vec<_>>() {
        visit(n, &by_name, &mut mark, &mut Vec::new(), &mut order)?;
    }
    Ok(order)
}

#[cfg(test)]
mod tests;
