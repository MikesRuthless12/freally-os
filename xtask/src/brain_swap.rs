// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask brain-swap <service> [--crash] [--out <file>]` (TASK-085):
//! builds the service for a dev image, signs its SHA-256 with the dev key,
//! and prints `swap_service`'s arguments as JSON, for Claude Code to call
//! the Brain with (or writes them to `--out`).
//!
//! `--crash` builds it with `swap-crash-test`: a build that ends a second
//! after it starts, to prove the automatic rollback (TASK-083). Only the
//! desktop has that feature; it never reaches an image.

use crate::build::{self, PROGRAMS};
use crate::keys::{self, KeyName};
use fcore_abi::Flavour;

/// The arguments for `swap_service`.
pub fn arguments(service: &str, crash: bool) -> Result<fjson::Value, String> {
    let program = PROGRAMS
        .iter()
        .find(|p| p.bin == service || p.path.rsplit('/').next() == Some(service))
        .ok_or_else(|| format!("no service is called {service}"))?;
    if crash && program.package != "svc-desktop" {
        return Err(String::from("--crash: only the desktop has a build made to crash"));
    }
    let elf = if crash {
        build::build_user_with(&[program], Flavour::Dev, &[&format!("{}/swap-crash-test", program.package)])?
    } else {
        build::build_user(&[program], Flavour::Dev)?
    };
    let path = &elf.first().ok_or("nothing was built")?.2;
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let sha = fcrypto::sha2::sha256(&bytes);
    let seed = keys::load_seed(KeyName::Dev)?;
    let signature = fcrypto::ed25519::sign(&seed, &sha);
    Ok(fjson::Value::obj([
        ("name", service.into()),
        ("binary_base64", fjson::base64::encode(&bytes).into()),
        ("signature_base64", fjson::base64::encode(&signature).into()),
    ]))
}

pub fn brain_swap_cmd(args: &[String]) -> Result<(), String> {
    let service = args
        .first()
        .filter(|a| !a.starts_with("--"))
        .ok_or("cargo xtask brain-swap <service> [--crash] [--out <file>]")?;
    let crash = args.iter().any(|a| a == "--crash");
    let v = arguments(service, crash)?;
    let json = v.to_json();
    match args.iter().position(|a| a == "--out") {
        Some(i) => {
            let out = args.get(i + 1).ok_or("--out needs a file")?;
            std::fs::write(out, &json).map_err(|e| format!("{out}: {e}"))?;
            eprintln!("brain-swap: {service}'s build, signed, {} bytes of arguments → {out}", json.len());
        }
        None => println!("{json}"),
    }
    Ok(())
}
