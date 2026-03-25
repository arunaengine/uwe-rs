//! Values read from YAML or JSON documents, each with the place it came from.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt;
use std::sync::Arc;

/// A place in a source document: its URL and a 1-based line and column.
/// Line 0 stands for the whole document, for errors such as a missing file.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Location {
    pub file: Arc<str>,
    pub line: u32,
    pub column: u32,
}

impl Location {
    pub fn new(file: &Arc<str>, line: u32, column: u32) -> Self {
        Self {
            file: Arc::clone(file),
            line,
            column,
        }
    }

    /// The location of a document as a whole.
    pub fn document(file: &Arc<str>) -> Self {
        Self::new(file, 0, 0)
    }
}

impl fmt::Display for Location {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let file = self.file.strip_prefix("file://").unwrap_or(&self.file);
        if self.line == 0 {
            write!(formatter, "{file}")
        } else {
            write!(formatter, "{file}:{}:{}", self.line, self.column)
        }
    }
}

/// A value together with the location where it starts.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub value: Value,
    pub location: Location,
}

/// A JSON-compatible value; maps keep the order of their entries.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Integer(i64),
    /// An integer beyond `i64`, as its decimal digits with an optional `-`, so that values
    /// like `4200000000000000000000000000000000000000000` keep every digit.
    BigInteger(String),
    Float(f64),
    String(String),
    List(Vec<Node>),
    Map(Vec<Entry>),
}

/// One entry of a map; its location is the location of the key.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub key: String,
    pub location: Location,
    pub value: Node,
}

impl Node {
    pub fn new(value: Value, location: Location) -> Self {
        Self { value, location }
    }

    pub fn as_str(&self) -> Option<&str> {
        match &self.value {
            Value::String(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Node]> {
        match &self.value {
            Value::List(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&[Entry]> {
        match &self.value {
            Value::Map(entries) => Some(entries),
            _ => None,
        }
    }

    /// The value of a map entry, when this node is a map that has the key.
    pub fn get(&self, key: &str) -> Option<&Node> {
        self.as_map()?
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| &entry.value)
    }

    /// What kind of value this is, worded for error messages.
    pub fn kind(&self) -> &'static str {
        match self.value {
            Value::Null => "null",
            Value::Bool(_) => "a boolean",
            Value::Integer(_) | Value::BigInteger(_) => "an integer",
            Value::Float(_) => "a number",
            Value::String(_) => "a string",
            Value::List(_) => "a list",
            Value::Map(_) => "a map",
        }
    }
}
