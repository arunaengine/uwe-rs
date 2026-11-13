//! CWL values: JSON data that keeps integers exact, and the text forms cwltool gives them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt::Write as _;
use std::sync::Arc;

use cwl_parser::{Node, yaml};
use indexmap::IndexMap;

/// A map of a CWL value; it keeps the order of its keys.
pub type Map = IndexMap<String, Value>;

/// An input, output or intermediate value, such as a job object or a `File`.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Integer(i64),
    /// An integer beyond `i64`, as its decimal digits with an optional `-`.
    BigInteger(String),
    Float(f64),
    String(String),
    List(Vec<Value>),
    Map(Map),
}

impl Value {
    /// The value of a node read from a document, a job file or JSON text.
    pub fn from_node(node: &Node) -> Value {
        match &node.value {
            cwl_parser::Value::Null => Value::Null,
            cwl_parser::Value::Bool(value) => Value::Bool(*value),
            cwl_parser::Value::Integer(value) => Value::Integer(*value),
            cwl_parser::Value::BigInteger(digits) => Value::BigInteger(digits.clone()),
            cwl_parser::Value::Float(value) => Value::Float(*value),
            cwl_parser::Value::String(text) => Value::String(text.clone()),
            cwl_parser::Value::List(items) => {
                Value::List(items.iter().map(Value::from_node).collect())
            }
            cwl_parser::Value::Map(entries) => Value::Map(
                entries
                    .iter()
                    .map(|entry| (entry.key.clone(), Value::from_node(&entry.value)))
                    .collect(),
            ),
        }
    }

    /// Reads JSON (or YAML) text; integers stay exact.
    pub fn parse(text: &str) -> Result<Value, String> {
        let node = yaml::read(text, &Arc::from("json")).map_err(|error| error.message)?;
        Ok(Value::from_node(&node))
    }

    pub fn string(text: impl Into<String>) -> Value {
        Value::String(text.into())
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&Map> {
        match self {
            Value::Map(map) => Some(map),
            _ => None,
        }
    }

    pub fn as_map_mut(&mut self) -> Option<&mut Map> {
        match self {
            Value::Map(map) => Some(map),
            _ => None,
        }
    }

    /// The value of a key, when this is a map that has it.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_map()?.get(key)
    }

    /// The text of a key, when this is a map whose value there is a string.
    pub fn text(&self, key: &str) -> Option<&str> {
        self.get(key)?.as_str()
    }

    /// `File` or `Directory` for file objects.
    pub fn class(&self) -> Option<&str> {
        self.text("class")
    }

    pub fn is_file(&self) -> bool {
        self.class() == Some("File")
    }

    pub fn is_directory(&self) -> bool {
        self.class() == Some("Directory")
    }

    /// A `File` or a `Directory` object.
    pub fn is_file_like(&self) -> bool {
        matches!(self.class(), Some("File" | "Directory"))
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Integer(value) => Some(*value),
            Value::Float(value) if value.fract() == 0.0 && value.abs() < 9.2e18 => {
                Some(*value as i64)
            }
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Integer(value) => Some(*value as f64),
            Value::BigInteger(digits) => digits.parse().ok(),
            Value::Float(value) => Some(*value),
            _ => None,
        }
    }

    /// Whether this is a number of any kind.
    pub fn is_number(&self) -> bool {
        matches!(
            self,
            Value::Integer(_) | Value::BigInteger(_) | Value::Float(_)
        )
    }

    /// Whether Python would count the value as true, as cwltool tests results.
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(value) => *value,
            Value::Integer(value) => *value != 0,
            Value::BigInteger(_) => true,
            Value::Float(value) => *value != 0.0,
            Value::String(text) => !text.is_empty(),
            Value::List(items) => !items.is_empty(),
            Value::Map(map) => !map.is_empty(),
        }
    }

    /// The value as Python's `json.dumps` writes it, with keys sorted when asked; this is how
    /// cwltool puts non-string values into interpolated strings.
    pub fn to_python_json(&self, sort_keys: bool) -> String {
        let mut text = String::new();
        write_json(self, &mut text, JsonStyle::Python { sort_keys });
        text
    }

    /// The value as compact JSON, with integers exact.
    pub fn to_json(&self) -> String {
        let mut text = String::new();
        write_json(self, &mut text, JsonStyle::Compact);
        text
    }

    /// The value as indented JSON, as cwltool prints the output object with
    /// `json.dumps(indent=4, ensure_ascii=False)`: in the order of its keys, and with every
    /// integer exact, also those beyond 64 bits.
    pub fn to_pretty_json(&self) -> String {
        let mut text = String::new();
        write_pretty(self, &mut text, 0);
        text
    }

    /// The value as a JavaScript literal: JSON, with `NaN` and `Infinity` for such numbers.
    pub fn to_javascript(&self) -> String {
        let mut text = String::new();
        write_json(self, &mut text, JsonStyle::Javascript);
        text
    }

    /// The value as Python's `str()` gives it, which is how cwltool writes command line
    /// arguments that are not files.
    pub fn to_python_text(&self) -> String {
        match self {
            Value::String(text) => text.clone(),
            other => python_repr(other),
        }
    }

    /// A short description of the value for messages, like ``a File `reads.fastq` `` or
    /// `the string "12"`, instead of its whole JSON.
    pub fn summary(&self) -> String {
        match self {
            Value::Null => "null".into(),
            Value::Bool(value) => format!("the boolean {value}"),
            Value::Integer(_) | Value::BigInteger(_) | Value::Float(_) => {
                format!("the number {}", self.to_json())
            }
            Value::String(text) => {
                let shown: String = text.chars().take(40).collect();
                let cut = if shown.len() < text.len() { "..." } else { "" };
                format!("the string {}{cut}", Value::String(shown).to_json())
            }
            Value::List(items) => match items.first() {
                None => "an empty list".into(),
                Some(first) => format!(
                    "a list of {} items (the first is {})",
                    items.len(),
                    first.summary()
                ),
            },
            Value::Map(_) if self.is_file_like() => {
                let class = self.class().unwrap_or("File");
                let name = ["basename", "location", "path"]
                    .iter()
                    .find_map(|key| self.text(key))
                    .and_then(|name| name.trim_end_matches('/').rsplit('/').next())
                    .filter(|name| !name.is_empty());
                match name {
                    Some(name) => format!("a {class} `{name}`"),
                    None => format!("a {class}"),
                }
            }
            Value::Map(_) => "an object".into(),
        }
    }
}

impl From<&str> for Value {
    fn from(text: &str) -> Value {
        Value::String(text.to_owned())
    }
}

impl From<String> for Value {
    fn from(text: String) -> Value {
        Value::String(text)
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Value {
        Value::Integer(value)
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Value {
        Value::Bool(value)
    }
}

impl From<Map> for Value {
    fn from(map: Map) -> Value {
        Value::Map(map)
    }
}

impl From<Vec<Value>> for Value {
    fn from(items: Vec<Value>) -> Value {
        Value::List(items)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JsonStyle {
    Compact,
    Javascript,
    Python { sort_keys: bool },
}

/// Writes indented JSON, four spaces per level.
fn write_pretty(value: &Value, text: &mut String, depth: usize) {
    let indent = |text: &mut String, depth: usize| text.push_str(&" ".repeat(4 * depth));
    match value {
        Value::List(items) if !items.is_empty() => {
            text.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    text.push_str(",\n");
                }
                indent(text, depth + 1);
                write_pretty(item, text, depth + 1);
            }
            text.push('\n');
            indent(text, depth);
            text.push(']');
        }
        Value::Map(map) if !map.is_empty() => {
            text.push_str("{\n");
            for (index, (key, item)) in map.iter().enumerate() {
                if index > 0 {
                    text.push_str(",\n");
                }
                indent(text, depth + 1);
                write_string(key, text, false);
                text.push_str(": ");
                write_pretty(item, text, depth + 1);
            }
            text.push('\n');
            indent(text, depth);
            text.push('}');
        }
        Value::Float(number) if number.is_finite() => text.push_str(&python_float(*number)),
        other => write_json(other, text, JsonStyle::Compact),
    }
}

fn write_json(value: &Value, text: &mut String, style: JsonStyle) {
    let python = matches!(style, JsonStyle::Python { .. });
    match value {
        Value::Null => text.push_str("null"),
        Value::Bool(value) => text.push_str(if *value { "true" } else { "false" }),
        Value::Integer(value) => {
            let _ = write!(text, "{value}");
        }
        Value::BigInteger(digits) => text.push_str(digits),
        Value::Float(value) => {
            if value.is_nan() {
                text.push_str(if style == JsonStyle::Compact {
                    "null"
                } else {
                    "NaN"
                });
            } else if value.is_infinite() {
                let name = match (style, *value > 0.0) {
                    (JsonStyle::Compact, _) => "null",
                    (_, true) => "Infinity",
                    (_, false) => "-Infinity",
                };
                text.push_str(name);
            } else if python {
                text.push_str(&python_float(*value));
            } else {
                text.push_str(&javascript_float(*value));
            }
        }
        Value::String(value) => write_string(value, text, python),
        Value::List(items) => {
            text.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    text.push_str(if python { ", " } else { "," });
                }
                write_json(item, text, style);
            }
            text.push(']');
        }
        Value::Map(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            if style == (JsonStyle::Python { sort_keys: true }) {
                entries.sort_by(|a, b| a.0.cmp(b.0));
            }
            text.push('{');
            for (index, (key, item)) in entries.into_iter().enumerate() {
                if index > 0 {
                    text.push_str(if python { ", " } else { "," });
                }
                write_string(key, text, python);
                text.push_str(if python { ": " } else { ":" });
                write_json(item, text, style);
            }
            text.push('}');
        }
    }
}

/// A JSON string; `ascii` escapes all non-ASCII characters, as Python's `json.dumps` does.
fn write_string(value: &str, text: &mut String, ascii: bool) {
    text.push('"');
    for character in value.chars() {
        match character {
            '"' => text.push_str("\\\""),
            '\\' => text.push_str("\\\\"),
            '\n' => text.push_str("\\n"),
            '\r' => text.push_str("\\r"),
            '\t' => text.push_str("\\t"),
            '\u{8}' => text.push_str("\\b"),
            '\u{c}' => text.push_str("\\f"),
            // Line and paragraph separators end lines in older JavaScript.
            '\u{2028}' | '\u{2029}' if !ascii => {
                let _ = write!(text, "\\u{:04x}", character as u32);
            }
            c if (c as u32) < 0x20 || (ascii && !c.is_ascii()) => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(text, "\\u{unit:04x}");
                }
            }
            c => text.push(c),
        }
    }
    text.push('"');
}

/// A float as Python's `repr` writes it: the shortest digits that read back the same, in
/// positional form from 1e-4 up to 1e16 and in exponent form otherwise.
pub fn python_float(value: f64) -> String {
    if value.is_nan() {
        return "nan".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf" } else { "-inf" }.into();
    }
    let (negative, digits, exponent) = decompose(value);
    let sign = if negative { "-" } else { "" };
    if (-5 < exponent) && (exponent < 16) {
        format!("{sign}{}", positional(&digits, exponent, true))
    } else {
        let mantissa = match digits.len() {
            1 => digits.clone(),
            _ => format!("{}.{}", &digits[..1], &digits[1..]),
        };
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        format!("{sign}{mantissa}e{exponent_sign}{:02}", exponent.abs())
    }
}

/// A float in positional form without trailing zeros, as cwltool writes floats on the command
/// line: `0.00001`, `0.0000123`, `123000`.
pub fn plain_float(value: f64) -> String {
    if !value.is_finite() {
        return python_float(value);
    }
    let (negative, digits, exponent) = decompose(value);
    let sign = if negative && value != 0.0 { "-" } else { "" };
    format!("{sign}{}", positional(&digits, exponent, false))
}

/// A float as JavaScript's `String(number)` writes it, which JSON and JavaScript both read.
pub fn javascript_float(value: f64) -> String {
    let (negative, digits, exponent) = decompose(value);
    let sign = if negative { "-" } else { "" };
    if (-7 < exponent) && (exponent < 21) {
        format!("{sign}{}", positional(&digits, exponent, false))
    } else {
        let mantissa = match digits.len() {
            1 => digits.clone(),
            _ => format!("{}.{}", &digits[..1], &digits[1..]),
        };
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        format!("{sign}{mantissa}e{exponent_sign}{}", exponent.abs())
    }
}

/// The sign, the shortest significant digits and the decimal exponent of the first digit.
fn decompose(value: f64) -> (bool, String, i32) {
    if value == 0.0 {
        return (value.is_sign_negative(), "0".into(), 0);
    }
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    (value < 0.0, digits, exponent.parse().unwrap_or(0))
}

/// Digits placed around the decimal point; `point_zero` adds `.0` to whole numbers.
fn positional(digits: &str, exponent: i32, point_zero: bool) -> String {
    let point = exponent + 1;
    if point <= 0 {
        return format!("0.{}{digits}", "0".repeat((-point) as usize));
    }
    let point = point as usize;
    if point >= digits.len() {
        let whole = format!("{digits}{}", "0".repeat(point - digits.len()));
        return if point_zero {
            format!("{whole}.0")
        } else {
            whole
        };
    }
    format!("{}.{}", &digits[..point], &digits[point..])
}

/// Python's `repr` of a value, for lists and maps inside command line arguments.
fn python_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Integer(value) => value.to_string(),
        Value::BigInteger(digits) => digits.clone(),
        Value::Float(value) => python_float(*value),
        Value::String(text) => {
            let quote = if text.contains('\'') && !text.contains('"') {
                '"'
            } else {
                '\''
            };
            let mut repr = String::from(quote);
            for character in text.chars() {
                match character {
                    '\\' => repr.push_str("\\\\"),
                    '\n' => repr.push_str("\\n"),
                    '\r' => repr.push_str("\\r"),
                    '\t' => repr.push_str("\\t"),
                    c if c == quote => {
                        repr.push('\\');
                        repr.push(c);
                    }
                    c => repr.push(c),
                }
            }
            repr.push(quote);
            repr
        }
        Value::List(items) => {
            let items: Vec<String> = items.iter().map(python_repr).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Map(map) => {
            let entries: Vec<String> = map
                .iter()
                .map(|(key, item)| {
                    format!(
                        "{}: {}",
                        python_repr(&Value::String(key.clone())),
                        python_repr(item)
                    )
                })
                .collect();
            format!("{{{}}}", entries.join(", "))
        }
    }
}

/// Whether two values are equal as Python compares JSON data: `1 == 1.0` and `True == 1`.
pub fn python_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::List(a), Value::List(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| python_equal(a, b))
        }
        (Value::Map(a), Value::Map(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, value)| b.get(key).is_some_and(|other| python_equal(value, other)))
        }
        (Value::BigInteger(a), Value::BigInteger(b)) => a == b,
        _ => match (number(a), number(b)) {
            (Some(a), Some(b)) => a == b,
            _ => a == b,
        },
    }
}

/// The number a scalar stands for in Python, where booleans are 0 and 1.
fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(value) => Some(f64::from(u8::from(*value))),
        other => other.as_f64(),
    }
}

#[cfg(test)]
#[path = "value_tests.rs"]
mod tests;
