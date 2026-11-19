// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask brain-swap <service> [--crash] [--send [--port N] | --out <file>]` (TASK-085):
//! builds the service for a dev image, signs its SHA-256 with the dev key,
//! and prints `swap_service`'s arguments as JSON, for Claude Code to call
//! the Brain with (or writes them to `--out`).
//!
//! `--send` swaps it in itself (TASK-086): through the running bridge's side
//! door (`brain-bridge`, port + 10) when Claude Code holds the line, else
//! straight to COM2, and says how it went. This is how Claude Code swaps a
//! service: a build is megabytes, more than a tool call can carry.
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

/// Swaps `service` in on the running dev VM: through the bridge's side door
/// if Claude Code holds the line, else straight to COM2. The Brain's answer.
pub fn send(service: &str, args: fjson::Value, port: u16) -> Result<String, String> {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;
    let side = port + brain_bridge::SIDE_OFFSET;
    let (mut s, via) = match TcpStream::connect(("127.0.0.1", side)) {
        Ok(s) => (s, format!("the bridge's side door ({side})")),
        Err(_) => {
            let s = TcpStream::connect(("127.0.0.1", port))
                .map_err(|e| format!("no bridge on {side} and no dev VM on {port}: {e}"))?;
            (s, format!("COM2 ({port})"))
        }
    };
    let call = fjson::Value::obj([
        ("jsonrpc", "2.0".into()),
        ("id", "side:swap".into()),
        ("method", "tools/call".into()),
        (
            "params",
            fjson::Value::obj([
                ("name", "swap_service".into()),
                ("arguments", args),
                ("_meta", fjson::Value::obj([("io.modelcontextprotocol/protocolVersion", "2026-07-28".into())])),
            ]),
        ),
    ]);
    eprintln!("brain-swap: sending {service}'s build through {via} (COM2 carries about 140 KB a second)");
    s.write_all(format!("\n{}\n", call.to_json()).as_bytes()).map_err(|e| e.to_string())?;
    let mut r = BufReader::new(s);
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Err(String::from("the line closed before the Brain answered"));
        }
        let Ok(v) = fjson::parse(line.trim()) else { continue };
        if v.get("id").and_then(fjson::Value::as_str) != Some("side:swap") {
            continue;
        }
        if let Some(e) = v.get("error") {
            return Err(format!("the Brain: {}", e.to_json()));
        }
        let res = v.get("result").ok_or("no result")?;
        let text = res
            .get("content")
            .and_then(fjson::Value::as_array)
            .and_then(|c| c.first())
            .and_then(|c| c.get("text"))
            .and_then(fjson::Value::as_str)
            .unwrap_or_default()
            .to_string();
        if res.get("isError").and_then(fjson::Value::as_bool) == Some(true) {
            return Err(format!("refused: {text}"));
        }
        return Ok(text);
    }
}

pub fn brain_swap_cmd(args: &[String]) -> Result<(), String> {
    let service = args
        .first()
        .filter(|a| !a.starts_with("--"))
        .ok_or("cargo xtask brain-swap <service> [--crash] [--send [--port N] | --out <file>]")?;
    let crash = args.iter().any(|a| a == "--crash");
    let v = arguments(service, crash)?;
    if args.iter().any(|a| a == "--send") {
        let port = brain_bridge::port_arg(args)?;
        let said = send(service, v, port)?;
        println!("brain-swap: {service} swapped in and kept through 5 s: {said}");
        return Ok(());
    }
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
