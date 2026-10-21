//! Values of Argo's expressions, printed the way Go prints them: `fmt.Sprintf("%v")` for
//! expression results and `encoding/json` for `toJson`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// A value as Go's `interface{}` holds it after decoding JSON or running expr.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Array(Vec<Value>),
    /// Go maps print and encode with sorted keys, so a sorted map keeps that order.
    Map(BTreeMap<String, Value>),
}

impl Value {
    pub fn string(text: impl Into<String>) -> Value {
        Value::String(text.into())
    }

    /// The Go type name, as expr's `type()` and error messages show it.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Nil => "nil",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Map(_) => "map",
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(text) => Some(text),
            _ => None,
        }
    }

    /// Decodes JSON the way Go decodes it into `interface{}`: every number is a float64.
    pub fn from_json(json: &serde_json::Value) -> Value {
        match json {
            serde_json::Value::Null => Value::Nil,
            serde_json::Value::Bool(flag) => Value::Bool(*flag),
            serde_json::Value::Number(number) => Value::Float(number.as_f64().unwrap_or(0.0)),
            serde_json::Value::String(text) => Value::String(text.clone()),
            serde_json::Value::Array(items) => {
                Value::Array(items.iter().map(Value::from_json).collect())
            }
            serde_json::Value::Object(entries) => Value::Map(
                entries
                    .iter()
                    .map(|(key, value)| (key.clone(), Value::from_json(value)))
                    .collect(),
            ),
        }
    }

    /// Parses JSON text into a value, or explains why it is not JSON.
    pub fn parse_json(text: &str) -> Result<Value, String> {
        serde_json::from_str::<serde_json::Value>(text)
            .map(|json| Value::from_json(&json))
            .map_err(|error| error.to_string())
    }

    /// The value as `fmt.Sprintf("%v", value)` prints it.
    pub fn to_go_string(&self) -> String {
        let mut text = String::new();
        self.write_go(&mut text);
        text
    }

    fn write_go(&self, out: &mut String) {
        match self {
            Value::Nil => out.push_str("<nil>"),
            Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
            Value::Int(number) => {
                let _ = write!(out, "{number}");
            }
            Value::Float(number) => out.push_str(&go_float(*number)),
            Value::String(text) => out.push_str(text),
            Value::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(' ');
                    }
                    item.write_go(out);
                }
                out.push(']');
            }
            Value::Map(entries) => {
                out.push_str("map[");
                for (index, (key, value)) in entries.iter().enumerate() {
                    if index > 0 {
                        out.push(' ');
                    }
                    out.push_str(key);
                    out.push(':');
                    value.write_go(out);
                }
                out.push(']');
            }
        }
    }

    /// The value as Go's `json.Marshal` writes it.
    pub fn to_json(&self) -> String {
        let mut text = String::new();
        self.write_json(&mut text);
        text
    }

    fn write_json(&self, out: &mut String) {
        match self {
            Value::Nil => out.push_str("null"),
            Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
            Value::Int(number) => {
                let _ = write!(out, "{number}");
            }
            Value::Float(number) => out.push_str(&argo_parser::model::json_float(*number)),
            Value::String(text) => json_string(text, out),
            Value::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    item.write_json(out);
                }
                out.push(']');
            }
            Value::Map(entries) => {
                out.push('{');
                for (index, (key, value)) in entries.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    json_string(key, out);
                    out.push(':');
                    value.write_json(out);
                }
                out.push('}');
            }
        }
    }

    /// The value as indented JSON, like expr's `toJSON`.
    pub fn to_json_indented(&self) -> String {
        let mut text = String::new();
        self.write_indented(&mut text, 0);
        text
    }

    fn write_indented(&self, out: &mut String, depth: usize) {
        let indent = |out: &mut String, depth: usize| {
            out.push('\n');
            out.push_str(&"  ".repeat(depth));
        };
        match self {
            Value::Array(items) if !items.is_empty() => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    indent(out, depth + 1);
                    item.write_indented(out, depth + 1);
                }
                indent(out, depth);
                out.push(']');
            }
            Value::Map(entries) if !entries.is_empty() => {
                out.push('{');
                for (index, (key, value)) in entries.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    indent(out, depth + 1);
                    json_string(key, out);
                    out.push_str(": ");
                    value.write_indented(out, depth + 1);
                }
                indent(out, depth);
                out.push('}');
            }
            other => other.write_json(out),
        }
    }
}

/// A JSON string as Go writes it, which also escapes `<`, `>` and `&` for HTML.
pub fn json_string(text: &str, out: &mut String) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' | '>' | '&' | '\u{2028}' | '\u{2029}' => {
                let _ = write!(out, "\\u{:04x}", character as u32);
            }
            control if (control as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", control as u32);
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

/// Text as Go's `strconv.Quote` writes it, with the quotes.
pub fn go_quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' | '\\' => {
                out.push('\\');
                out.push(character);
            }
            '\u{7}' => out.push_str("\\a"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{b}' => out.push_str("\\v"),
            ' ' => out.push(' '),
            control if control < ' ' || control == '\u{7f}' => {
                let _ = write!(out, "\\x{:02x}", control as u32);
            }
            // Go prints other characters that are not graphic, like U+00A0, as escapes.
            other if other.is_control() || other.is_whitespace() => {
                let code = other as u32;
                if code < 0x10000 {
                    let _ = write!(out, "\\u{code:04x}");
                } else {
                    let _ = write!(out, "\\U{code:08x}");
                }
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// A float as Go's `%v` prints it, which is `strconv.FormatFloat(f, 'g', -1, 64)`: the
/// shortest digits that read back the same, with an exponent when it is below -4 or from 6 on,
/// so a million prints as `1e+06`.
pub fn go_float(number: f64) -> String {
    if number.is_nan() {
        return "NaN".into();
    }
    if number.is_infinite() {
        return if number > 0.0 { "+Inf" } else { "-Inf" }.into();
    }
    if number == 0.0 {
        return if number.is_sign_negative() { "-0" } else { "0" }.into();
    }
    // Rust's `{:e}` gives the shortest digits, like `1.25e-7`.
    let scientific = format!("{:e}", number.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let sign = if number < 0.0 { "-" } else { "" };
    if !(-4..6).contains(&exponent) {
        let mut text = format!("{sign}{}", &digits[..1]);
        if digits.len() > 1 {
            text.push('.');
            text.push_str(&digits[1..]);
        }
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        let _ = write!(text, "e{exponent_sign}{:02}", exponent.abs());
        return text;
    }
    format!("{sign}{}", number.abs())
}

#[cfg(test)]
#[path = "value_tests.rs"]
mod tests;
