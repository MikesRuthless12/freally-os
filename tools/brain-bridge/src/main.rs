// SPDX-License-Identifier: Apache-2.0
//! `brain-bridge [--port N]`: the MCP server Claude Code launches (usually
//! as `cargo xtask brain-bridge`). See the library.

use std::net::{Ipv4Addr, SocketAddr};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let port = match brain_bridge::port_arg(&args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("brain-bridge: {e}");
            std::process::exit(2);
        }
    };
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let r = brain_bridge::run(addr, std::io::stdin(), std::io::stdout(), &mut std::io::stderr(), Default::default());
    if let Err(e) = r {
        eprintln!("brain-bridge: {e}");
        std::process::exit(1);
    }
}
