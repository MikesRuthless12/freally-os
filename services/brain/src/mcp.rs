// SPDX-License-Identifier: Apache-2.0
//! MCP over JSON-RPC 2.0 (TASK-080), written from the specification with no
//! SDK. A dual-era server, as the 2026-07-28 revision's *Versioning* page
//! describes:
//!
//! - **modern** (2026-07-28): every request carries its protocol version in
//!   `_meta["io.modelcontextprotocol/protocolVersion"]`; no handshake;
//!   `server/discover`; results say `"resultType": "complete"`; the live log
//!   is followed with `subscriptions/listen`;
//! - **legacy** (2025-11-25 and the three revisions before it): `initialize`
//!   picks the version for the rest of the line; `resources/subscribe`.
//!
//! One line in, the lines to send back out: no I/O here, so the transcripts
//! in `tests/` replay it on the host.

use crate::schema;
use crate::tools::{self, ToolDef};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use fjson::Value;

/// The revision this server speaks per request.
pub const MODERN: &str = "2026-07-28";
/// The handshake revisions it also serves, newest first.
pub const LEGACY: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
pub const SERVER_NAME: &str = "freally-brain";

const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_SUBSCRIPTION: &str = "io.modelcontextprotocol/subscriptionId";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
/// The 2026-07-28 revision's UnsupportedProtocolVersionError.
const UNSUPPORTED_VERSION: i64 = -32022;
/// Legacy revisions' "resource not found" (2026-07-28 uses INVALID_PARAMS).
const LEGACY_NOT_FOUND: i64 = -32002;

const INSTRUCTIONS: &str = "The developer build of Freally OS, running. Read it with system_status, boot_timeline, \
list_processes, list_services, read_log, get_faults and screenshot; change it with restart_service, swap_service \
(the arguments come from `cargo xtask brain-swap <service>`), rollback_service, send_input, set_theme and reboot. \
Every call is written to the OS's log.";

/// What a tool answered.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolAnswer {
    /// MCP content items (`text`, `image`).
    pub content: Vec<Value>,
    /// The same answer as data, for tools that return JSON.
    pub structured: Option<Value>,
    /// The tool ran and failed: the model reads why and may try again.
    pub is_error: bool,
}

impl ToolAnswer {
    /// A JSON answer: as data, and as its text for clients that read only text.
    pub fn json(v: Value) -> ToolAnswer {
        let text = v.to_json();
        // structuredContent is an object; a list goes inside one.
        let structured = match v {
            Value::Obj(_) => v,
            other => Value::obj([("items", other)]),
        };
        ToolAnswer { content: vec![text_item(&text)], structured: Some(structured), is_error: false }
    }

    /// A PNG, base64 already.
    pub fn png(base64: String) -> ToolAnswer {
        let item = Value::obj([("type", "image".into()), ("data", base64.into()), ("mimeType", "image/png".into())]);
        ToolAnswer { content: vec![item], structured: None, is_error: false }
    }

    /// The tool failed; `why` is for the model to read.
    pub fn error(why: &str) -> ToolAnswer {
        ToolAnswer { content: vec![text_item(why)], structured: None, is_error: true }
    }
}

fn text_item(t: &str) -> Value {
    Value::obj([("type", "text".into()), ("text", t.into())])
}

/// What the Brain reads and does, behind the protocol.
pub trait Host {
    /// Runs a tool whose arguments already passed its schema.
    fn call(&mut self, tool: &ToolDef, args: &Value) -> ToolAnswer;
    /// A resource's MIME type and text, or `None` if there's no such resource.
    fn read(&mut self, uri: &str) -> Option<(&'static str, String)>;
    /// The services, by name (each has a manifest resource).
    fn services(&mut self) -> Vec<String>;
}

/// Who wants to hear the live log grow.
#[derive(Clone, Debug, PartialEq)]
enum Follower {
    /// `resources/subscribe` (legacy).
    Legacy,
    /// `subscriptions/listen`, by its request id (modern).
    Stream(Value),
}

/// The server's state on one line: the legacy version, once `initialize`
/// picked one, and who follows the live log.
pub struct Server {
    version: String,
    legacy: Option<&'static str>,
    followers: Vec<Follower>,
}

enum Era {
    Modern,
    Legacy,
}

/// A JSON-RPC error, before its id is attached.
struct Fail {
    code: i64,
    message: String,
    data: Option<Value>,
}

fn fail(code: i64, message: &str) -> Fail {
    Fail { code, message: message.to_string(), data: None }
}

impl Server {
    /// `version` is the image's version (`serverInfo.version`).
    pub fn new(version: &str) -> Server {
        Server { version: version.to_string(), legacy: None, followers: Vec::new() }
    }

    /// One line from the client; the lines to send back (none for a
    /// notification).
    pub fn handle(&mut self, line: &[u8], host: &mut impl Host) -> Vec<String> {
        let Ok(text) = core::str::from_utf8(line) else {
            return vec![error_line(&Value::Null, fail(PARSE_ERROR, "Parse error: not UTF-8"))];
        };
        let text = text.trim();
        if text.is_empty() {
            return Vec::new();
        }
        let msg = match fjson::parse(text) {
            Ok(m) => m,
            Err(_) => return vec![error_line(&Value::Null, fail(PARSE_ERROR, "Parse error"))],
        };
        let id = msg.get("id").cloned();
        if !matches!(msg, Value::Obj(_)) || msg.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            // Batches went out of MCP in 2025-06-18.
            return vec![error_line(&id.unwrap_or(Value::Null), fail(INVALID_REQUEST, "Invalid Request"))];
        }
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            // A response: the Brain sends no requests, so there's nothing to match.
            return Vec::new();
        };
        let params = msg.get("params").cloned().unwrap_or(Value::Obj(Vec::new()));
        let Some(id) = id else {
            self.notification(method, &params);
            return Vec::new();
        };
        if !matches!(id, Value::Str(_) | Value::Num(_)) {
            return vec![error_line(
                &Value::Null,
                fail(INVALID_REQUEST, "Invalid Request: the id must be a string or a number"),
            )];
        }
        if !matches!(params, Value::Obj(_)) {
            return vec![error_line(&id, fail(INVALID_PARAMS, "Invalid params: not an object"))];
        }
        if method == "initialize" {
            return vec![self.initialize(&id, &params)];
        }
        let era = match self.era(&params) {
            Ok(e) => e,
            // ping is answered in any era (legacy allows it before initialize).
            Err(_) if method == "ping" => Era::Legacy,
            Err(f) => return vec![error_line(&id, f)],
        };
        let mut out = Vec::new();
        let answer = match method {
            "ping" => Ok(Value::Obj(Vec::new())),
            "server/discover" => Ok(self.discover()),
            "tools/list" => Ok(tools_list()),
            "tools/call" => tools_call(&params, host),
            "resources/list" => Ok(resources_list(host)),
            "resources/templates/list" => Ok(templates_list()),
            "resources/read" => resources_read(&params, host, &era),
            "resources/subscribe" | "resources/unsubscribe" => self.subscribe(method, &params),
            "subscriptions/listen" => match self.listen(&id, &params) {
                // The stream's answer is its acknowledgment; the request
                // itself stays open until the client cancels it.
                Ok(ack) => {
                    out.push(ack);
                    return out;
                }
                Err(f) => Err(f),
            },
            _ => Err(fail(METHOD_NOT_FOUND, &format!("Method not found: {method}"))),
        };
        out.push(match answer {
            Ok(mut result) => {
                if let Era::Modern = era {
                    result = complete(result);
                }
                result_line(&id, result)
            }
            Err(f) => error_line(&id, f),
        });
        out
    }

    /// The live log grew: a notification for each follower.
    pub fn log_grew(&self) -> Vec<String> {
        self.followers
            .iter()
            .map(|f| {
                let mut params = Value::obj([("uri", tools::LOG_LIVE.into())]);
                if let Follower::Stream(id) = f {
                    params = Value::obj([
                        ("_meta", Value::obj([(META_SUBSCRIPTION, id.clone())])),
                        ("uri", tools::LOG_LIVE.into()),
                    ]);
                }
                notification_line("notifications/resources/updated", params)
            })
            .collect()
    }

    /// Anyone following the live log.
    pub fn following(&self) -> bool {
        !self.followers.is_empty()
    }

    fn era(&self, params: &Value) -> Result<Era, Fail> {
        let asked = params.get("_meta").and_then(|m| m.get(META_VERSION));
        match (asked, self.legacy) {
            (Some(Value::Str(v)), _) if v == MODERN => Ok(Era::Modern),
            (None, Some(_)) => Ok(Era::Legacy),
            (asked, _) => {
                let mut data = Value::obj([("supported", supported())]);
                if let Some(a) = asked {
                    data.set("requested", a.clone());
                }
                Err(Fail {
                    code: UNSUPPORTED_VERSION,
                    message: "Unsupported protocol version".into(),
                    data: Some(data),
                })
            }
        }
    }

    fn initialize(&mut self, id: &Value, params: &Value) -> String {
        let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
        // The legacy rule: the client's version if the server has it, else
        // the server's newest; the client then decides whether to go on.
        let v = LEGACY.iter().copied().find(|l| *l == asked).unwrap_or(LEGACY[0]);
        self.legacy = Some(v);
        let result = Value::obj([
            ("protocolVersion", v.into()),
            ("capabilities", capabilities()),
            ("serverInfo", self.server_info()),
            ("instructions", INSTRUCTIONS.into()),
        ]);
        result_line(id, result)
    }

    fn discover(&self) -> Value {
        Value::obj([
            ("supportedVersions", supported()),
            ("capabilities", capabilities()),
            ("_meta", Value::obj([("io.modelcontextprotocol/serverInfo", self.server_info())])),
            ("instructions", INSTRUCTIONS.into()),
        ])
    }

    fn server_info(&self) -> Value {
        Value::obj([("name", SERVER_NAME.into()), ("version", self.version.as_str().into())])
    }

    fn notification(&mut self, method: &str, params: &Value) {
        // A cancelled stream stops; anything else (initialized, a cancelled
        // call that has already been answered) needs nothing.
        if method == "notifications/cancelled"
            && let Some(id) = params.get("requestId")
        {
            self.followers.retain(|f| *f != Follower::Stream(id.clone()));
        }
    }

    fn subscribe(&mut self, method: &str, params: &Value) -> Result<Value, Fail> {
        let uri =
            params.get("uri").and_then(Value::as_str).ok_or_else(|| fail(INVALID_PARAMS, "Invalid params: uri"))?;
        if uri != tools::LOG_LIVE {
            return Err(fail(INVALID_PARAMS, &format!("Only {} can be subscribed to", tools::LOG_LIVE)));
        }
        self.followers.retain(|f| *f != Follower::Legacy);
        if method == "resources/subscribe" {
            self.followers.push(Follower::Legacy);
        }
        Ok(Value::Obj(Vec::new()))
    }

    fn listen(&mut self, id: &Value, params: &Value) -> Result<String, Fail> {
        let wanted =
            params.get("notifications").ok_or_else(|| fail(INVALID_PARAMS, "Invalid params: notifications"))?;
        // Only the live log changes; every other kind is left out of the
        // acknowledgment, as the spec says.
        let log = wanted
            .get("resourceSubscriptions")
            .and_then(Value::as_array)
            .is_some_and(|uris| uris.iter().any(|u| u.as_str() == Some(tools::LOG_LIVE)));
        let mut agreed = Value::Obj(Vec::new());
        if log {
            agreed.set("resourceSubscriptions", Value::Arr(vec![tools::LOG_LIVE.into()]));
            self.followers.push(Follower::Stream(id.clone()));
        }
        Ok(notification_line(
            "notifications/subscriptions/acknowledged",
            Value::obj([("_meta", Value::obj([(META_SUBSCRIPTION, id.clone())])), ("notifications", agreed)]),
        ))
    }
}

fn supported() -> Value {
    Value::Arr(core::iter::once(MODERN).chain(LEGACY.iter().copied()).map(Value::from).collect())
}

/// Legacy clients subscribe with `resources/subscribe`; modern ones with
/// `subscriptions/listen`; the same flag announces both.
fn capabilities() -> Value {
    Value::obj([("tools", Value::Obj(Vec::new())), ("resources", Value::obj([("subscribe", true.into())]))])
}

/// A modern result: `resultType` first.
fn complete(result: Value) -> Value {
    let mut fields = vec![(String::from("resultType"), Value::str("complete"))];
    if let Value::Obj(rest) = result {
        fields.extend(rest);
    }
    Value::Obj(fields)
}

fn tools_list() -> Value {
    let list = tools::TOOLS
        .iter()
        .map(|t| {
            Value::obj([
                ("name", t.name.into()),
                ("title", t.title.into()),
                ("description", t.description.into()),
                ("inputSchema", fjson::parse(t.schema).unwrap_or(Value::Null)),
            ])
        })
        .collect();
    Value::obj([("tools", Value::Arr(list))])
}

fn tools_call(params: &Value, host: &mut impl Host) -> Result<Value, Fail> {
    let name =
        params.get("name").and_then(Value::as_str).ok_or_else(|| fail(INVALID_PARAMS, "Invalid params: name"))?;
    let tool = tools::find(name).ok_or_else(|| fail(INVALID_PARAMS, &format!("Unknown tool: {name}")))?;
    let args = params.get("arguments").cloned().unwrap_or(Value::Obj(Vec::new()));
    let answer = match fjson::parse(tool.schema) {
        Ok(schema) => match schema::check(&schema, &args) {
            Ok(()) => host.call(tool, &args),
            Err(why) => ToolAnswer::error(&format!("Invalid arguments: {why}")),
        },
        Err(_) => ToolAnswer::error("The tool's own schema doesn't parse"),
    };
    let mut result = Value::obj([("content", Value::Arr(answer.content))]);
    if let Some(s) = answer.structured {
        result.set("structuredContent", s);
    }
    result.set("isError", answer.is_error.into());
    Ok(result)
}

fn resources_list(host: &mut impl Host) -> Value {
    let mut list: Vec<Value> = tools::RESOURCES
        .iter()
        .map(|r| {
            Value::obj([
                ("uri", r.uri.into()),
                ("name", r.name.into()),
                ("description", r.description.into()),
                ("mimeType", r.mime.into()),
            ])
        })
        .collect();
    for s in host.services() {
        list.push(Value::obj([
            ("uri", tools::MANIFEST_TEMPLATE.replace("{name}", &s).into()),
            ("name", format!("{s}'s manifest").into()),
            ("mimeType", "text/plain".into()),
        ]));
    }
    Value::obj([("resources", Value::Arr(list))])
}

fn templates_list() -> Value {
    Value::obj([(
        "resourceTemplates",
        Value::Arr(vec![Value::obj([
            ("uriTemplate", tools::MANIFEST_TEMPLATE.into()),
            ("name", "A service's manifest".into()),
            ("mimeType", "text/plain".into()),
        ])]),
    )])
}

fn resources_read(params: &Value, host: &mut impl Host, era: &Era) -> Result<Value, Fail> {
    let uri = params.get("uri").and_then(Value::as_str).ok_or_else(|| fail(INVALID_PARAMS, "Invalid params: uri"))?;
    match host.read(uri) {
        Some((mime, text)) => Ok(Value::obj([(
            "contents",
            Value::Arr(vec![Value::obj([("uri", uri.into()), ("mimeType", mime.into()), ("text", text.into())])]),
        )])),
        None => Err(Fail {
            code: if matches!(era, Era::Modern) { INVALID_PARAMS } else { LEGACY_NOT_FOUND },
            message: "Resource not found".into(),
            data: Some(Value::obj([("uri", uri.into())])),
        }),
    }
}

fn result_line(id: &Value, result: Value) -> String {
    Value::obj([("jsonrpc", "2.0".into()), ("id", id.clone()), ("result", result)]).to_json()
}

fn error_line(id: &Value, f: Fail) -> String {
    let mut e = Value::obj([("code", f.code.into()), ("message", f.message.into())]);
    if let Some(d) = f.data {
        e.set("data", d);
    }
    Value::obj([("jsonrpc", "2.0".into()), ("id", id.clone()), ("error", e)]).to_json()
}

fn notification_line(method: &str, params: Value) -> String {
    Value::obj([("jsonrpc", "2.0".into()), ("method", method.into()), ("params", params)]).to_json()
}

/// The log line for a tool call (FR-035: `brain: <tool> <args summary>`):
/// each argument, a long string as its length only (a swap's binary, typed
/// text), the whole at most 200 characters.
pub fn summary(tool: &str, args: &Value) -> String {
    let mut s = format!("brain: {tool}");
    if let Value::Obj(fields) = args {
        for (k, v) in fields {
            let shown = match v {
                Value::Str(t) if t.chars().count() > 48 => format!("<{} characters>", t.chars().count()),
                other => other.to_json(),
            };
            s.push_str(&format!(" {k}={shown}"));
        }
    }
    if s.chars().count() > 200 {
        s = s.chars().take(199).collect();
        s.push('…');
    }
    s
}
