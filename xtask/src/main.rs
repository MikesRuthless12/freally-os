// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask`: Freally OS's build tool. Host only, `std` only, no crate from
//! outside this workspace.

mod atlas;
mod build;
mod cdp;
mod doctor;
mod gates;
mod keys;
mod pack;
mod run;
mod screen;
mod util;

use std::process::ExitCode;

const HELP: &str = "\
cargo xtask <command>

  doctor                         is this machine ready? (KVM, QEMU, OVMF, toolchain, keys)
  keygen --dev --staging         make the signing keys in ~/.freally-keys/ (never --release)
  build --flavour F              the bootloader and the signed image for release, dev or e2e
  run --flavour F [--headless]   build and boot it in QEMU (KVM), COM1 log, QMP socket
  cdp --os [--site] [--only=ids] the CDP legs: every coverage.json feature, spec by spec
  pack --flavour F [--out P]     pack an unsigned image
  sign <image> --key dev|staging sign an image in place
  verify <image>                 verify an image against the dev and staging keys
  atlas [--check]                render the boot atlas from the Noto fonts (or check it)
  screen title|integrity --out P draw a pre-desktop screen to a PNG (as the bootloader does)
  gate <name>                    run one gate: third-party
";

fn verify_cmd(args: &[String]) -> Result<(), String> {
    let path = args.first().ok_or("cargo xtask verify <image>")?;
    let image = std::fs::read(path).map_err(|e| format!("can't read {path}: {e}"))?;
    let trusted = [keys::load_public(keys::KeyName::Dev)?, keys::load_public(keys::KeyName::Staging)?];
    let v = fsi::verify(&image, &trusted).map_err(|e| format!("{path}: refused: {} ({e:?})", e.describe()))?;
    println!(
        "verify: {path}: Freally OS {} ({}), signed by the {} key, {} files",
        v.header.version_str(),
        fcore_abi::Flavour::from_u32(v.header.flavour as u32).map(|f| f.name()).unwrap_or("?"),
        ["dev", "staging"][v.key_index],
        v.toc().count()
    );
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or(&[]);
    let result = match args.first().map(String::as_str) {
        Some("doctor") => doctor::doctor(),
        Some("keygen") => keys::keygen(rest),
        Some("build") => build::build_cmd(rest),
        Some("pack") => pack::pack_cmd(rest),
        Some("sign") => pack::sign_cmd(rest),
        Some("verify") => verify_cmd(rest),
        Some("atlas") => atlas::atlas_cmd(rest),
        Some("screen") => screen::screen_cmd(rest),
        Some("run") => run::run_cmd(rest),
        Some("cdp") => cdp::cdp_cmd(rest),
        Some("gate") => match rest.first().map(String::as_str) {
            Some("third-party") => gates::third_party::check().map(|s| println!("third-party: {s}")),
            other => Err(format!("unknown gate {other:?}\n{HELP}")),
        },
        Some("help") | Some("--help") | None => {
            print!("{HELP}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command {other:?}\n{HELP}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}
