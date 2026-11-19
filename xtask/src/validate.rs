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
/// Words only conductor's swap (dev images) says (`services/conductor/src/swap.rs`).
const SWAP_MARKER: &[u8] = b"only the Brain swaps services";

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

    // The Brain (TASK-088): in dev images alone, and conductor's swap with it.
    let brain = paths.iter().find(|p| p.ends_with("/brain"));
    match (f, brain) {
        (Flavour::Dev, None) => {
            return Err("the dev image has no Brain: the other flavours' check would prove nothing".into());
        }
        (Flavour::Dev, Some(_)) => {}
        (_, Some(p)) => return Err(format!("the {name} image carries {p}")),
        (_, None) => out.push(format!("{name}: no Brain in the image")),
    }
    let (_, conductor) = fsi::recheck_file(&image, "/system/conductor")
        .map_err(|e| format!("{name} image's conductor: {}", e.describe()))?;
    match (f, contains(conductor, SWAP_MARKER)) {
        (Flavour::Dev, true) => out.push("dev: the Brain, and conductor's swap (the other flavours' control)".into()),
        (Flavour::Dev, false) => {
            return Err("the dev conductor can't swap: the other flavours' check would prove nothing".into());
        }
        (_, true) => return Err(format!("the {name} conductor has the swap compiled in")),
        (_, false) => out.push(format!("{name}: no swap in conductor")),
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
    out.push(no_writes(&manifests, &paths, &interlock_tested()).map_err(|e| format!("{name} image: {e}"))?);
    Ok(out)
}

/// Storage drivers by the binary's name: each must carry the partition
/// interlock's test (TASK-092) before an image may hold it.
const STORAGE_DRIVERS: [&str; 6] = ["nvme", "ahci", "ata", "virtio-blk", "usb-storage", "sdhci"];

/// The storage drivers whose partition interlock test is in the tree: the
/// file `drivers/<name>/src/interlock.rs` with a test refusing a write
/// outside the Freally partitions (`AccessDenied`).
pub fn interlock_tested() -> Vec<String> {
    let root = crate::util::repo_root();
    STORAGE_DRIVERS
        .iter()
        .filter(|d| {
            std::fs::read_to_string(root.join("drivers").join(d).join("src/interlock.rs"))
                .is_ok_and(|s| s.contains("#[test]") && s.contains("AccessDenied"))
        })
        .map(|d| d.to_string())
        .collect()
}

/// The no-writes audit (TASK-077, PRD charter rule 6): nothing in an image
/// can write to a storage device unless the partition interlock stands in
/// its way.
/// - no storage driver, unless its interlock test is in the tree (`allowed`);
/// - the root resource (`devices`: what carves a controller's registers)
///   only for conductor (PID 1, which hands devmgr its share) and devmgr,
///   so no other service can reach a disk;
/// - the DMA grant (a disk is written by DMA) only for an allowed storage
///   driver (and conductor, PID 1, whose manifest lists every grant because
///   it holds them all to hand out; it drives no device).
pub fn no_writes(manifests: &[fsvc::Manifest], paths: &[String], allowed: &[String]) -> Result<String, String> {
    let storage = |s: &str| STORAGE_DRIVERS.iter().find(|d| s.contains(*d)).map(|d| d.to_string());
    for p in paths.iter().filter(|p| p.starts_with("/system/drivers/")) {
        if let Some(d) = storage(p)
            && !allowed.contains(&d)
        {
            return Err(format!("it carries the storage driver {p} with no partition interlock test (TASK-077)"));
        }
    }
    for m in manifests {
        if m.grants.iter().any(|g| g == "devices") && !["conductor", "devmgr"].contains(&m.name.as_str()) {
            return Err(format!("{} holds the root resource; only conductor and devmgr may (TASK-077)", m.name));
        }
        if m.grants.iter().any(|g| g == "dma")
            && m.name != "conductor"
            && !storage(&m.binary).is_some_and(|d| allowed.contains(&d))
        {
            return Err(format!("{} holds the DMA grant without a tested partition interlock (TASK-077)", m.name));
        }
    }
    let drivers = paths.iter().filter(|p| p.starts_with("/system/drivers/")).count();
    Ok(match allowed {
        [] => format!(
            "no writes: no storage driver among its {drivers} drivers, the root resource conductor's and devmgr's alone, no DMA"
        ),
        a => format!("no writes outside Freally partitions: {} only with the interlock tested", a.join(", ")),
    })
}

/// Freally Core's ELF, as the image carries it.
fn kernel(image: &[u8]) -> Result<&[u8], String> {
    fsi::recheck_file(image, "/system/kernel").map(|(_, k)| k).map_err(|e| e.describe().to_string())
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::no_writes;

    fn m(name: &str, binary: &str, grants: &str) -> fsvc::Manifest {
        let text = format!(
            "name = {name}\nbinary = {binary}\ndescription = A test service for the no-writes rule.\nstart = boot\nrestart = never\nclass = normal\ngrants = {grants}\nflavours = release\n"
        );
        fsvc::parse(&text).unwrap()
    }

    fn paths(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn todays_images_pass() {
        let ms = [m("devmgr", "/system/services/devmgr", "devices"), m("logger", "/system/services/logger", "serial")];
        let ps = paths(&["/system/drivers/gop-fb", "/system/drivers/i8042", "/system/services/devmgr"]);
        assert!(no_writes(&ms, &ps, &[]).unwrap().contains("no storage driver among its 2 drivers"));
    }

    #[test]
    fn a_storage_driver_without_its_interlock_is_refused() {
        let ps = paths(&["/system/drivers/nvme"]);
        assert!(no_writes(&[], &ps, &[]).unwrap_err().contains("storage driver /system/drivers/nvme"));
        // With its interlock tested, and its DMA grant, it may.
        let ms = [m("drv-nvme", "/system/drivers/nvme", "dma")];
        assert!(no_writes(&ms, &ps, &["nvme".to_string()]).is_ok());
    }

    #[test]
    fn the_root_resource_and_dma_stay_where_they_belong() {
        let ms = [m("desktop", "/system/services/desktop", "devices")];
        assert!(no_writes(&ms, &[], &[]).unwrap_err().contains("desktop holds the root resource"));
        let ms = [m("drv-x", "/system/drivers/x", "dma")];
        assert!(no_writes(&ms, &[], &[]).unwrap_err().contains("drv-x holds the DMA grant"));
    }
}
