// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask keygen`: the dev and staging signing keys, in `~/.freally-keys/`.
//!
//! - A key file holds a 32-byte Ed25519 seed as hex, mode 0600, in a 0700 folder.
//! - Keys are never overwritten (`--force` to replace one, which breaks every image
//!   signed with the old one) and never written inside the repository.
//! - `--release` is refused: the release key is made only in Phase 7's key
//!   ceremony, offline, onto its own USB drives (TASK-109).
//! - The public halves are written to `boot/src/keys.rs`, which is committed, so
//!   every machine builds the same bootloader.

use crate::util;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

const MAGIC: &str = "freally-ed25519-seed-v1";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyName {
    Dev,
    Staging,
}

impl KeyName {
    pub fn as_str(self) -> &'static str {
        match self {
            KeyName::Dev => "dev",
            KeyName::Staging => "staging",
        }
    }

    pub fn secret_path(self) -> PathBuf {
        match (self, std::env::var_os("FREALLY_DEV_KEY")) {
            (KeyName::Dev, Some(p)) => PathBuf::from(p),
            _ => util::keys_dir().join(format!("{}.key", self.as_str())),
        }
    }

    pub fn public_path(self) -> PathBuf {
        util::keys_dir().join(format!("{}.pub", self.as_str()))
    }
}

pub fn keygen(args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "--release") {
        return Err("the release key is made only in Phase 7's key ceremony (TASK-109), offline, \
                    onto its own USB drives; never here"
            .into());
    }
    let force = args.iter().any(|a| a == "--force");
    let mut names = Vec::new();
    if args.iter().any(|a| a == "--dev") {
        names.push(KeyName::Dev);
    }
    if args.iter().any(|a| a == "--staging") {
        names.push(KeyName::Staging);
    }
    if names.is_empty() {
        return Err("say which: cargo xtask keygen --dev --staging".into());
    }
    let dir = util::keys_dir();
    if dir.starts_with(util::repo_root()) {
        return Err("the keys folder must be outside the repository".into());
    }
    fs::create_dir_all(&dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;

    for name in names {
        let path = name.secret_path();
        if path.exists() && !force {
            println!("keygen: {} already exists; kept (use --force to replace it)", path.display());
            continue;
        }
        let mut seed = [0u8; 32];
        fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut seed))
            .map_err(|e| format!("can't read /dev/urandom: {e}"))?;
        let text = format!(
            "# Freally OS {} signing key (an Ed25519 seed). Never commit it, never copy it off this machine.\n{MAGIC} {}\n",
            name.as_str(),
            util::hex(&seed)
        );
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| format!("can't write {}: {e}", path.display()))?;
        std::io::Write::write_all(&mut f, text.as_bytes()).map_err(|e| e.to_string())?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
        let public = fcrypto::ed25519::public_key(&seed);
        fs::write(name.public_path(), format!("{}\n", util::hex(&public))).map_err(|e| e.to_string())?;
        println!("keygen: {} key written to {} (public {})", name.as_str(), path.display(), util::hex(&public));
    }
    write_keys_rs()
}

/// Reads a secret seed, refusing a file anyone but its owner can read.
pub fn load_seed(name: KeyName) -> Result<[u8; 32], String> {
    let path = name.secret_path();
    let meta = fs::metadata(&path)
        .map_err(|_| format!("no {} key at {} (run: cargo xtask keygen --{})", name.as_str(), path.display(), name.as_str()))?;
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(format!("{} is readable by others; chmod 600 it", path.display()));
    }
    let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let hex = text
        .lines()
        .find_map(|l| l.strip_prefix(MAGIC))
        .ok_or_else(|| format!("{} isn't a Freally key file", path.display()))?;
    util::unhex(hex)?.try_into().map_err(|_| format!("{}: the seed isn't 32 bytes", path.display()))
}

pub fn load_public(name: KeyName) -> Result<[u8; 32], String> {
    Ok(fcrypto::ed25519::public_key(&load_seed(name)?))
}

fn read_public(name: KeyName) -> Option<[u8; 32]> {
    let text = fs::read_to_string(name.public_path()).ok()?;
    util::unhex(text.trim()).ok()?.try_into().ok()
}

fn rust_array(k: &[u8; 32]) -> String {
    let parts: Vec<String> = k.iter().map(|b| format!("0x{b:02x}")).collect();
    let mut s = String::from("[\n");
    for chunk in parts.chunks(8) {
        s.push_str("    ");
        s.push_str(&chunk.join(", "));
        s.push_str(",\n");
    }
    s.push(']');
    s
}

/// Writes `boot/src/keys.rs` from the public keys in `~/.freally-keys/`.
pub fn write_keys_rs() -> Result<(), String> {
    let dev = read_public(KeyName::Dev).ok_or("no dev public key; run cargo xtask keygen --dev")?;
    let staging = read_public(KeyName::Staging).ok_or("no staging public key; run cargo xtask keygen --staging")?;
    let text = format!(
        "// SPDX-License-Identifier: Apache-2.0
//! The public keys a bootloader may trust. Written by `cargo xtask keygen`; the
//! secret halves never leave Mike's machine (`~/.freally-keys/`).
//!
//! - A **dev** or **e2e** bootloader trusts the dev and staging keys.
//! - A **release** bootloader trusts only the staging key until `v0.8.0`, then
//!   only the release key, made in Phase 7's key ceremony (TASK-109).

/// The dev key: signs dev and e2e images.
pub const DEV: [u8; 32] = {dev};

/// The staging key: signs release-flavour images until the release key exists.
pub const STAGING: [u8; 32] = {staging};
",
        dev = rust_array(&dev),
        staging = rust_array(&staging)
    );
    let path = util::repo_root().join("boot/src/keys.rs");
    if fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
        fs::write(&path, text).map_err(|e| e.to_string())?;
        println!("keygen: boot/src/keys.rs updated (commit it)");
    }
    Ok(())
}
