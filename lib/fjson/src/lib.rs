// SPDX-License-Identifier: Apache-2.0
//! Freally's own JSON (RFC 8259) and Base64 (RFC 4648 § 4).
//!
//! - Objects keep their keys in order (a `Vec`, not a map), so output is stable.
//! - Parsing is strict (no trailing commas, no comments) and depth-limited, so a
//!   hostile document can't exhaust the stack.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod base64;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

/// The deepest nesting the parser accepts.
pub const MAX_DEPTH: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

impl Value {
    /// An object from `(key, value)` pairs.
    pub fn obj<const N: usize>(pairs: [(&str, Value); N]) -> Value {
        Value::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    pub fn str(s: &str) -> Value {
        Value::Str(s.to_string())
    }

    /// The member `key` of an object.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            _ => None,
        }
    }

    /// The number, if it's a whole number that fits.
    pub fn as_i64(&self) -> Option<i64> {
        let n = self.as_f64()?;
        if whole(n) && (-9_007_199_254_740_992.0..=9_007_199_254_740_992.0).contains(&n) {
            Some(n as i64)
        } else {
            None
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Arr(a) => Some(a),
            _ => None,
        }
    }

    /// Sets (or replaces) `key` in an object.
    pub fn set(&mut self, key: &str, v: Value) {
        if let Value::Obj(m) = self {
            if let Some(slot) = m.iter_mut().find(|(k, _)| k == key) {
                slot.1 = v;
            } else {
                m.push((key.to_string(), v));
            }
        }
    }

    /// Compact JSON text.
    pub fn to_json(&self) -> String {
        let mut s = String::new();
        self.write(&mut s);
        s
    }

    fn write(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Num(n) => {
                if !n.is_finite() {
                    out.push_str("null");
                } else if whole(*n) && *n > -1e15 && *n < 1e15 {
                    let _ = write!(out, "{}", *n as i64);
                } else {
                    let _ = write!(out, "{n}");
                }
            }
            Value::Str(s) => write_str(out, s),
            Value::Arr(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Value::Obj(m) => {
                out.push('{');
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_str(out, k);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Value {
        Value::Bool(b)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Value {
        Value::str(s)
    }
}

impl From<String> for Value {
    fn from(s: String) -> Value {
        Value::Str(s)
    }
}

macro_rules! from_num {
    ($($t:ty),*) => { $(impl From<$t> for Value { fn from(n: $t) -> Value { Value::Num(n as f64) } })* };
}
from_num!(i32, i64, u8, u16, u32, u64, usize, f32, f64);

/// `true` for a finite whole number (no `f64::fract` in `core`).
fn whole(n: f64) -> bool {
    n.is_finite() && (n as i64) as f64 == n
}

fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Why a document didn't parse, and where (a byte offset).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ParseError {
    pub at: usize,
    pub what: &'static str,
}

/// Parses one JSON document (surrounding whitespace allowed, nothing else).
pub fn parse(text: &str) -> Result<Value, ParseError> {
    let mut p = Parser { b: text.as_bytes(), i: 0 };
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err(p.err("text after the document"));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn err(&self, what: &'static str) -> ParseError {
        ParseError { at: self.i, what }
    }

    fn ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn lit(&mut self, word: &[u8], v: Value) -> Result<Value, ParseError> {
        if self.b.get(self.i..self.i + word.len()) == Some(word) {
            self.i += word.len();
            Ok(v)
        } else {
            Err(self.err("unknown word"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, ParseError> {
        if depth > MAX_DEPTH {
            return Err(self.err("nested too deeply"));
        }
        self.ws();
        match self.b.get(self.i) {
            None => Err(self.err("unexpected end")),
            Some(b'n') => self.lit(b"null", Value::Null),
            Some(b't') => self.lit(b"true", Value::Bool(true)),
            Some(b'f') => self.lit(b"false", Value::Bool(false)),
            Some(b'"') => Ok(Value::Str(self.string()?)),
            Some(b'[') => {
                self.i += 1;
                let mut a = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Value::Arr(a));
                }
                loop {
                    a.push(self.value(depth + 1)?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Value::Arr(a));
                        }
                        _ => return Err(self.err("expected , or ]")),
                    }
                }
            }
            Some(b'{') => {
                self.i += 1;
                let mut m = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Value::Obj(m));
                }
                loop {
                    self.ws();
                    if self.b.get(self.i) != Some(&b'"') {
                        return Err(self.err("expected a key"));
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return Err(self.err("expected :"));
                    }
                    self.i += 1;
                    let v = self.value(depth + 1)?;
                    m.push((k, v));
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Value::Obj(m));
                        }
                        _ => return Err(self.err("expected , or }")),
                    }
                }
            }
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.err("unexpected character")),
        }
    }

    fn number(&mut self) -> Result<Value, ParseError> {
        let start = self.i;
        if self.b.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        match self.b.get(self.i) {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => {
                while matches!(self.b.get(self.i), Some(b'0'..=b'9')) {
                    self.i += 1;
                }
            }
            _ => return Err(self.err("bad number")),
        }
        if self.b.get(self.i) == Some(&b'.') {
            self.i += 1;
            if !matches!(self.b.get(self.i), Some(b'0'..=b'9')) {
                return Err(self.err("bad fraction"));
            }
            while matches!(self.b.get(self.i), Some(b'0'..=b'9')) {
                self.i += 1;
            }
        }
        if matches!(self.b.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.b.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if !matches!(self.b.get(self.i), Some(b'0'..=b'9')) {
                return Err(self.err("bad exponent"));
            }
            while matches!(self.b.get(self.i), Some(b'0'..=b'9')) {
                self.i += 1;
            }
        }
        let text = core::str::from_utf8(&self.b[start..self.i]).map_err(|_| self.err("bad number"))?;
        text.parse::<f64>().map(Value::Num).map_err(|_| self.err("bad number"))
    }

    fn hex4(&mut self) -> Result<u32, ParseError> {
        let h = self.b.get(self.i..self.i + 4).ok_or(self.err("short \\u escape"))?;
        let s = core::str::from_utf8(h).map_err(|_| self.err("bad \\u escape"))?;
        let v = u32::from_str_radix(s, 16).map_err(|_| self.err("bad \\u escape"))?;
        self.i += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, ParseError> {
        self.i += 1; // the opening quote
        let mut s = String::new();
        loop {
            let start = self.i;
            while let Some(&c) = self.b.get(self.i) {
                if c == b'"' || c == b'\\' || c < 0x20 {
                    break;
                }
                self.i += 1;
            }
            s.push_str(core::str::from_utf8(&self.b[start..self.i]).map_err(|_| self.err("not UTF-8"))?);
            match self.b.get(self.i) {
                Some(b'"') => {
                    self.i += 1;
                    return Ok(s);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let e = *self.b.get(self.i).ok_or(self.err("unexpected end"))?;
                    self.i += 1;
                    match e {
                        b'"' => s.push('"'),
                        b'\\' => s.push('\\'),
                        b'/' => s.push('/'),
                        b'b' => s.push('\u{8}'),
                        b'f' => s.push('\u{c}'),
                        b'n' => s.push('\n'),
                        b'r' => s.push('\r'),
                        b't' => s.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let cp = if (0xD800..0xDC00).contains(&hi) {
                                if self.b.get(self.i..self.i + 2) != Some(b"\\u") {
                                    return Err(self.err("lone surrogate"));
                                }
                                self.i += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err(self.err("bad surrogate pair"));
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else {
                                hi
                            };
                            s.push(char::from_u32(cp).ok_or(self.err("bad code point"))?);
                        }
                        _ => return Err(self.err("bad escape")),
                    }
                }
                _ => return Err(self.err("unterminated string")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_cdp_message() {
        let text = r#"{"id":7,"method":"Page.captureScreenshot","params":{"format":"png","clip":{"x":1.5,"y":0,"scale":1}},"ok":true,"none":null,"list":[1,-2,"three"]}"#;
        let v = parse(text).unwrap();
        assert_eq!(v.get("id").and_then(Value::as_i64), Some(7));
        assert_eq!(
            v.get("params").and_then(|p| p.get("clip")).and_then(|c| c.get("x")).and_then(Value::as_f64),
            Some(1.5)
        );
        assert_eq!(v.to_json(), text);
    }

    #[test]
    fn strings_escape_and_unescape() {
        let v = Value::str("a\"b\\c\nd\u{1}é😀");
        let text = v.to_json();
        assert_eq!(text, "\"a\\\"b\\\\c\\nd\\u0001é😀\"");
        assert_eq!(parse(&text).unwrap(), v);
        assert_eq!(parse(r#""\ud83d\ude00\u00e9""#).unwrap(), Value::str("😀é"));
    }

    #[test]
    fn rejects_what_rfc_8259_rejects() {
        for bad in [
            "",
            "{",
            "[1,]",
            "{\"a\":1,}",
            "01",
            "1.",
            "-",
            "\"\\x\"",
            "\"\\ud800\"",
            "nul",
            "[1] 2",
            "{a:1}",
            "\"a\nb\"",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn deep_nesting_is_refused_not_a_crash() {
        let deep = "[".repeat(10_000) + &"]".repeat(10_000);
        assert_eq!(parse(&deep).unwrap_err().what, "nested too deeply");
    }
}
