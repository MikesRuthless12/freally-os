// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask validate` (TASK-046, FR-037): the flavours stay apart, proved
//! on what the build made, not on what the code says.
//!
//! - **release:** the image carries no `brain`, `cdpd` or `uart`; Freally
//!   Core has no `debug_log` (it's stripped, so the dev-only path is looked
//!   for by its marker text); the bootloader embeds exactly one known key,
//!   never the dev key; the header says release (0) and the staging key.
//! - **dev, e2e:** the header says flavour 1 or 2 and the dev key; the dev
//!   kernel has the `debug_log` marker (else the release check proves nothing).
//! - **every image:** its manifests parse and check as a set, and each names
//!   a binary the image carries and lists the image's flavour.
//!
//! Every check here was watched failing on a planted build.

use crate::build::{self, Built};
use crate::keys::{self, KeyName};
use fcore_abi::Flavour;

/// Paths a release image never carries.
const DEV_ONLY: [&str; 3] = ["brain", "cdpd", "uart"];

pub fn validate_cmd(args: &[String]) -> Result<(), String> {
    let signed = !args.iter().any(|a| a == "--unsigned");
    for line in validate(signed)? {
        println!("validate: {line}");
    }
    Ok(())
}

/// Builds the three flavours and checks each; the lines say what was proved.
pub fn validate(signed: bool) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for f in [Flavour::Release, Flavour::Dev, Flavour::E2e] {
        let b = build::build_with(f, signed)?;
        out.extend(check(f, &b)?);
    }
    Ok(out)
}

/// FR-037 and the manifest rules for one flavour's build.
pub fn check(f: Flavour, b: &Built) -> Result<Vec<String>, String> {
    let name = f.name();
    let image = std::fs::read(&b.image).map_err(|e| format!("{}: {e}", b.image.display()))?;
    let header = fsi::Header::parse(&image).map_err(|e| format!("{name} image: {}", e.describe()))?;
    let paths: Vec<String> = fsi::recheck_toc(&image)
        .map_err(|e| format!("{name} image: {}", e.describe()))?
        .map(|e| e.path().to_string())
        .collect();
    let mut out = Vec::new();
    let (want_flavour, want_key) = match f {
        Flavour::Release => (0, KeyName::Staging),
        Flavour::Dev => (1, KeyName::Dev),
        Flavour::E2e => (2, KeyName::Dev),
    };
    if header.flavour != want_flavour {
        return Err(format!("the {name} image says flavour {}, not {want_flavour}", header.flavour));
    }
    if header.key_id != fsi::key_id(&keys::committed_public(want_key)?) {
        return Err(format!("the {name} image isn't for the {} key", want_key.as_str()));
    }
    out.push(format!("{name}: flavour {want_flavour}, the {} key", want_key.as_str()));

    if f == Flavour::Release {
        if let Some(p) = paths.iter().find(|p| DEV_ONLY.iter().any(|d| p.contains(d))) {
            return Err(format!("the release image carries {p}"));
        }
        out.push("release: no brain, cdpd or uart in the image".into());
        if contains(kernel(&image)?, b"debug_log") {
            return Err("the release kernel has debug_log in it".into());
        }
        out.push("release: no debug_log in Freally Core".into());
        let efi = std::fs::read(&b.efi).map_err(|e| format!("{}: {e}", b.efi.display()))?;
        let embedded: Vec<KeyName> = [KeyName::Dev, KeyName::Staging]
            .into_iter()
            .filter(|k| keys::committed_public(*k).is_ok_and(|p| contains(&efi, &p)))
            .collect();
        match embedded.as_slice() {
            [KeyName::Staging] => out.push("release: the bootloader embeds one key, staging".into()),
            other => {
                let names: Vec<&str> = other.iter().map(|k| k.as_str()).collect();
                return Err(format!("the release bootloader embeds {names:?}, not the staging key alone"));
            }
        }
    }

    if f == Flavour::Dev {
        if !contains(kernel(&image)?, b"debug_log") {
            return Err("the dev kernel lacks the debug_log marker: the release check would prove nothing".into());
        }
        out.push("dev: debug_log is in Freally Core (the release check's control)".into());
    }

    // The manifest rules, on the image as packed.
    let mut manifests = Vec::new();
    for p in paths.iter().filter(|p| p.starts_with("/system/manifests/")) {
        let (_, bytes) = fsi::recheck_file(&image, p).map_err(|e| format!("{p}: {}", e.describe()))?;
        let text = std::str::from_utf8(bytes).map_err(|_| format!("{name} image: {p} isn't UTF-8"))?;
        let m = fsvc::parse(text).map_err(|e| format!("{name} image: {p}: {e}"))?;
        if !paths.contains(&m.binary) {
            return Err(format!("{name} image: {p} names {}, which the image doesn't carry", m.binary));
        }
        let as_manifest = match f {
            Flavour::Release => fsvc::Flavour::Release,
            Flavour::Dev => fsvc::Flavour::Dev,
            Flavour::E2e => fsvc::Flavour::E2e,
        };
        if !m.flavours.contains(&as_manifest) {
            return Err(format!("{name} image: {p} isn't for {name}"));
        }
        manifests.push(m);
    }
    fsvc::check_set(&manifests).map_err(|e| format!("{name} image's manifests: {e}"))?;
    out.push(format!(
        "{name}: {} manifests, each for this flavour, each binary carried, the set in order",
        manifests.len()
    ));
    Ok(out)
}

/// Freally Core's ELF, as the image carries it.
fn kernel(image: &[u8]) -> Result<&[u8], String> {
    fsi::recheck_file(image, "/system/kernel").map(|(_, k)| k).map_err(|e| e.describe().to_string())
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}
