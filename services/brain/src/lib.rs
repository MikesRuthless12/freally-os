// SPDX-License-Identifier: Apache-2.0
//! The Brain (Phase 5, FR-035; the dev flavour only, compiled out of release
//! and e2e): the MCP server Claude Code talks to over COM2, through
//! `brain-bridge`. Ordinary code, no AI inside.
//!
//! This library is its protocol, host-tested against recorded transcripts:
//! - `mcp`: JSON-RPC 2.0 and MCP, both eras of the specification: the
//!   2026-07-28 revision (the protocol version on every request's `_meta`,
//!   `server/discover`) and the `initialize` handshake of 2025-11-25 and
//!   earlier, which Claude Code may still speak;
//! - `schema`: each tool's input checked against its JSON Schema before it
//!   runs;
//! - `tools`: the thirteen tools of PRD § 4, as the owner signed them off
//!   (2026-11-18), and the resources;
//! - `transport`: the newline-delimited line on COM2;
//! - `input`: `send_input`'s text and chords as key events.
#![no_std]

extern crate alloc;

pub mod input;
pub mod mcp;
pub mod schema;
pub mod tools;
pub mod transport;
