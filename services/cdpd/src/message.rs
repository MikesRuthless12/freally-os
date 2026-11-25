// SPDX-License-Identifier: Apache-2.0
//! A CDP message over cdpd's port (TASK-184), as DevTools frames it: a
//! command `{"id", "method", "params"}` in, `{"id", "result"}` or
//! `{"id", "error": {"code", "message"}}` out (JSON-RPC's server error code).

use alloc::string::String;
use alloc::vec::Vec;
use fjson::Value;

/// The error code every refusal carries (JSON-RPC 2.0's "server error").
pub const SERVER_ERROR: f64 = -32000.0;

/// `text`'s command run by `call`, its answer as JSON.
pub fn answer(text: &str, call: &mut dyn FnMut(&str, &Value) -> Result<Value, String>) -> String {
    let msg = fjson::parse(text);
    let id = msg.as_ref().ok().and_then(|m| m.get("id")).cloned().unwrap_or(Value::Null);
    let r = match &msg {
        Err(e) => Err(alloc::format!("not JSON: {e:?}")),
        Ok(m) => match m.get("method").and_then(Value::as_str) {
            None => Err(String::from("no method")),
            Some(method) => call(method, m.get("params").unwrap_or(&Value::Obj(Vec::new()))),
        },
    };
    match r {
        Ok(v) => Value::obj([("id", id), ("result", v)]).to_json(),
        Err(why) => Value::obj([
            ("id", id),
            ("error", Value::obj([("code", Value::Num(SERVER_ERROR)), ("message", Value::Str(why))])),
        ])
        .to_json(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_answered_by_their_id() {
        let mut call = |method: &str, params: &Value| -> Result<Value, String> {
            match method {
                "Echo" => Ok(params.clone()),
                _ => Err(alloc::format!("{method} isn't served")),
            }
        };
        assert_eq!(answer(r#"{"id":7,"method":"Echo","params":{"a":1}}"#, &mut call), r#"{"id":7,"result":{"a":1}}"#);
        assert_eq!(
            answer(r#"{"id":8,"method":"Echo"}"#, &mut call),
            r#"{"id":8,"result":{}}"#,
            "no params: an empty object"
        );
        assert_eq!(
            answer(r#"{"id":9,"method":"Nope"}"#, &mut call),
            r#"{"id":9,"error":{"code":-32000,"message":"Nope isn't served"}}"#
        );
        assert_eq!(answer(r#"{"id":10}"#, &mut call), r#"{"id":10,"error":{"code":-32000,"message":"no method"}}"#);
        assert!(answer("{not json", &mut call).starts_with(r#"{"id":null,"error":{"code":-32000,"message":"not JSON"#));
    }
}
