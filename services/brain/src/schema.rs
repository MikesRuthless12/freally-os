// SPDX-License-Identifier: Apache-2.0
//! Each tool's arguments checked against its input schema before it runs
//! (TASK-080): the part of JSON Schema (2020-12) the Brain's schemas use.
//! `type` (object, string, integer, number, boolean, array), `properties`,
//! `required`, `additionalProperties: false`, `enum`, `minimum`, `maximum`,
//! `minLength`, `maxLength`, `items`. A value that breaks one is refused
//! with the path to it and what's wrong, never half-used.

use alloc::format;
use alloc::string::String;
use fjson::Value;

/// Checks `value` against `schema`; the error says where and what.
pub fn check(schema: &Value, value: &Value) -> Result<(), String> {
    at(schema, value, "arguments")
}

fn type_ok(t: &str, v: &Value) -> bool {
    match (t, v) {
        ("object", Value::Obj(_)) | ("string", Value::Str(_)) | ("boolean", Value::Bool(_)) => true,
        ("array", Value::Arr(_)) | ("null", Value::Null) => true,
        ("number", Value::Num(_)) => true,
        ("integer", Value::Num(n)) => n.is_finite() && *n == (*n as i64) as f64,
        _ => false,
    }
}

fn at(schema: &Value, v: &Value, path: &str) -> Result<(), String> {
    if let Some(t) = schema.get("type").and_then(Value::as_str)
        && !type_ok(t, v)
    {
        return Err(format!("{path} must be {}", article(t)));
    }
    if let Some(options) = schema.get("enum").and_then(Value::as_array)
        && !options.contains(v)
    {
        let names: alloc::vec::Vec<String> = options.iter().map(Value::to_json).collect();
        return Err(format!("{path} must be one of {}", names.join(", ")));
    }
    if let Value::Num(n) = v {
        if let Some(min) = schema.get("minimum").and_then(Value::as_f64)
            && *n < min
        {
            return Err(format!("{path} must be at least {min}"));
        }
        if let Some(max) = schema.get("maximum").and_then(Value::as_f64)
            && *n > max
        {
            return Err(format!("{path} must be at most {max}"));
        }
    }
    if let Value::Str(s) = v {
        let len = s.chars().count() as f64;
        if let Some(min) = schema.get("minLength").and_then(Value::as_f64)
            && len < min
        {
            return Err(format!("{path} must be at least {min} characters"));
        }
        if let Some(max) = schema.get("maxLength").and_then(Value::as_f64)
            && len > max
        {
            return Err(format!("{path} must be at most {max} characters"));
        }
    }
    if let Value::Arr(items) = v
        && let Some(item) = schema.get("items")
    {
        for (i, x) in items.iter().enumerate() {
            at(item, x, &format!("{path}[{i}]"))?;
        }
    }
    if let Value::Obj(fields) = v {
        let props = schema.get("properties");
        for need in schema.get("required").and_then(Value::as_array).unwrap_or_default() {
            if let Some(name) = need.as_str()
                && !fields.iter().any(|(k, _)| k == name)
            {
                return Err(format!("{path}.{name} is required"));
            }
        }
        for (k, x) in fields {
            match props.and_then(|p| p.get(k)) {
                Some(sub) => at(sub, x, &format!("{path}.{k}"))?,
                None if schema.get("additionalProperties").and_then(Value::as_bool) == Some(false) => {
                    return Err(format!("{path}.{k} isn't an argument this tool takes"));
                }
                None => {}
            }
        }
    }
    Ok(())
}

fn article(t: &str) -> String {
    match t {
        "object" | "array" | "integer" => format!("an {t}"),
        _ => format!("a {t}"),
    }
}

#[cfg(test)]
mod tests {
    use super::check;
    use fjson::parse;

    #[test]
    fn arguments_are_checked_against_the_schema() {
        let s = parse(
            r#"{"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":1000},"min_level":{"enum":["trace","debug","info","warn","error"]},"text":{"type":"string","maxLength":4}},"required":["limit"],"additionalProperties":false}"#,
        )
        .unwrap();
        let ok = |j: &str| check(&s, &parse(j).unwrap());
        assert_eq!(ok(r#"{"limit":10,"min_level":"warn"}"#), Ok(()));
        assert_eq!(ok(r#"{}"#), Err("arguments.limit is required".into()));
        assert_eq!(ok(r#"{"limit":0}"#), Err("arguments.limit must be at least 1".into()));
        assert_eq!(ok(r#"{"limit":2.5}"#), Err("arguments.limit must be an integer".into()));
        assert!(ok(r#"{"limit":1,"min_level":"loud"}"#).unwrap_err().contains("must be one of"));
        assert_eq!(ok(r#"{"limit":1,"text":"hello"}"#), Err("arguments.text must be at most 4 characters".into()));
        assert_eq!(ok(r#"{"limit":1,"extra":true}"#), Err("arguments.extra isn't an argument this tool takes".into()));
        assert_eq!(ok(r#"[]"#), Err("arguments must be an object".into()));
    }
}
