// SPDX-License-Identifier: Apache-2.0
//! TASK-080's proof: recorded MCP transcripts replayed against the Brain's
//! protocol core. In each file under `transcripts/`:
//! - `> ` a line the client sent;
//! - `< ` a line the Brain must send back, in order (compared as JSON);
//! - `! log` the live log grew;
//! - `#` a comment.
//!
//! The tools behind it here are a stand-in: the transcripts prove the
//! protocol, and `cargo xtask brain-test` proves the tools on a dev VM.

use fjson::Value;
use svc_brain::mcp::{Host, Server, ToolAnswer};
use svc_brain::tools::ToolDef;

struct Stand;

impl Host for Stand {
    fn call(&mut self, tool: &ToolDef, _args: &Value) -> ToolAnswer {
        match tool.name {
            "system_status" => ToolAnswer::json(fjson::parse(r#"{"version":"0.4.0","uptime_ms":1234}"#).unwrap()),
            _ => ToolAnswer::error("not in these transcripts"),
        }
    }

    fn read(&mut self, uri: &str) -> Option<(&'static str, String)> {
        (uri == "freally://boot/timeline").then(|| ("application/json", r#"[{"stage":"B0","ms_from_b0":0}]"#.into()))
    }

    fn services(&mut self) -> Vec<String> {
        vec!["desktop".into()]
    }
}

fn replay(name: &str) {
    let path = format!("{}/tests/transcripts/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap();
    let mut server = Server::new("0.4.0");
    let mut sent: Vec<String> = Vec::new();
    let mut at = 0;
    let mut checked = 0;
    for (n, line) in text.lines().enumerate() {
        let n = n + 1;
        if let Some(req) = line.strip_prefix("> ").or(line.strip_prefix(">").filter(|r| r.is_empty())) {
            assert_eq!(
                at,
                sent.len(),
                "{name}:{n}: the Brain sent more than the transcript expects: {:?}",
                &sent[at..]
            );
            sent = server.handle(req.as_bytes(), &mut Stand);
            at = 0;
        } else if line == "! log" {
            assert_eq!(at, sent.len(), "{name}:{n}: unexpected lines {:?}", &sent[at..]);
            sent = server.log_grew();
            at = 0;
        } else if let Some(want) = line.strip_prefix("< ") {
            let got = sent.get(at).unwrap_or_else(|| panic!("{name}:{n}: the Brain sent nothing, expected {want}"));
            let (g, w) = (fjson::parse(got).unwrap(), fjson::parse(want).unwrap());
            assert_eq!(g, w, "{name}:{n}:\n got  {got}\n want {want}");
            at += 1;
            checked += 1;
        }
    }
    assert_eq!(at, sent.len(), "{name}: unexpected lines at the end: {:?}", &sent[at..]);
    assert!(checked > 0, "{name}: no answers checked");
}

#[test]
fn claude_code_modern() {
    replay("claude-code-modern.txt");
}

#[test]
fn claude_code_legacy() {
    replay("claude-code-legacy.txt");
}

#[test]
fn spec_edges() {
    replay("spec-edges.txt");
}
