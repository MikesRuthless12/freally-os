// SPDX-License-Identifier: Apache-2.0
//! `brain-bridge [--port N]`: the MCP server Claude Code launches (usually
//! as `cargo xtask brain-bridge`). See the library.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = brain_bridge::serve(&args) {
        eprintln!("brain-bridge: {e}");
        std::process::exit(1);
    }
}
