//! CWL values: JSON data that keeps integers exact, and the text forms cwltool gives them.
// Copyright (c) 2026 The iunctus Contributors
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
