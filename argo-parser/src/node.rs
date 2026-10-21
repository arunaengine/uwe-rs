//! Values of a parsed document, each with the position where it starts.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;

/// A line and a column in a file, both counted from 1. Columns count characters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Position {
    pub line: u32,
    pub column: u32,
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

/// A value with its position. Kubernetes fields that Argo passes on unchanged keep this form.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub value: Value,
    pub position: Position,
}

/// A YAML value after resolving plain scalars the way Argo's YAML library does (YAML 1.1).
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Integer(i64),
    /// Integers above `i64::MAX`, which YAML allows up to `u64::MAX`.
    Unsigned(u64),
    Float(f64),
    String(String),
    Sequence(Vec<Node>),
    Mapping(Vec<Entry>),
}

/// A mapping entry. Keys are strings, as in the JSON that Argo decodes after reading YAML.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub key: String,
    pub position: Position,
    pub value: Node,
}

impl Node {
    pub fn new(value: Value, position: Position) -> Self {
        Self { value, position }
    }

    /// The value of a mapping entry, if this is a mapping that has the key.
    pub fn get(&self, key: &str) -> Option<&Node> {
        match &self.value {
            Value::Mapping(entries) => entries
                .iter()
                .find(|entry| entry.key == key)
                .map(|entry| &entry.value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match &self.value {
            Value::String(text) => Some(text),
            _ => None,
        }
    }

    /// Names the kind of value for error messages, like "a sequence".
    pub fn describe(&self) -> &'static str {
        match self.value {
            Value::Null => "null",
            Value::Bool(_) => "a boolean",
            Value::Integer(_) | Value::Unsigned(_) => "an integer",
            Value::Float(_) => "a number",
            Value::String(_) => "a string",
            Value::Sequence(_) => "a sequence",
            Value::Mapping(_) => "a mapping",
        }
    }
}
