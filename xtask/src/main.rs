// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask`: Freally OS's build tool. Host only, `std` only, no crate from
//! outside this workspace.

mod doctor;
mod gates;
mod util;

use std::process::ExitCode;

const HELP: &str = "\
cargo xtask <command>

  doctor                 is this machine ready? (KVM, QEMU, OVMF, toolchain, keys)
  gate <name>            run one gate: third-party
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("doctor") => doctor::doctor(),
        Some("gate") => match args.get(1).map(String::as_str) {
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
