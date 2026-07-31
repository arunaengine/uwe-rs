//! The values of config files as a tree in which every value knows its file and line, and how
//! the trees of several files merge.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::Arc;

use toml::Spanned;
use toml::de::{DeTable, DeValue};

use crate::ConfigError;

/// Where a value was written: its file and line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    pub file: Arc<Path>,
    /// The line, from 1; 0 for a value that no file wrote.
    pub line: usize,
}

impl Origin {
    /// The origin of the empty settings without any file.
    pub(crate) fn nowhere() -> Origin {
        Origin {
            file: Arc::from(Path::new("")),
            line: 0,
        }
    }

    /// The first line of a file.
    pub(crate) fn start(file: &Path) -> Origin {
        Origin {
            file: Arc::from(file),
            line: 1,
        }
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.file.display(), self.line)
    }
}

/// A value and where it was written.
#[derive(Clone, Debug)]
pub(crate) struct Node {
    pub value: Value,
    pub origin: Origin,
}

#[derive(Clone, Debug)]
pub(crate) enum Value {
    Text(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    /// A date or time, which no setting takes yet.
    Datetime,
    Array(Vec<Node>),
    Table(BTreeMap<String, Node>),
}

impl Value {
    /// What kind of value this is, in words for errors.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Text(_) => "text",
            Value::Integer(_) => "a whole number",
            Value::Float(_) => "a number",
            Value::Boolean(_) => "true or false",
            Value::Datetime => "a date",
            Value::Array(_) => "a list",
            Value::Table(_) => "a table",
        }
    }
}

/// Merges later entries over earlier ones: tables merge key by key, and any other value
/// replaces the earlier one. A merged table keeps the origin of its first definition.
pub(crate) fn merge(entries: &mut BTreeMap<String, Node>, later: BTreeMap<String, Node>) {
    for (key, node) in later {
        match entries.get_mut(&key) {
            Some(existing) => match (&mut existing.value, node.value) {
                (Value::Table(earlier), Value::Table(more)) => merge(earlier, more),
                (_, value) => {
                    existing.value = value;
                    existing.origin = node.origin;
                }
            },
            None => {
                entries.insert(key, node);
            }
        }
    }
}

/// The top-level entries of a TOML text, read as the file `file`.
pub(crate) fn parse(text: &str, file: &Path) -> Result<BTreeMap<String, Node>, ConfigError> {
    let file: Arc<Path> = Arc::from(file);
    let origin = |offset: usize| Origin {
        file: file.clone(),
        line: text[..offset.min(text.len())].matches('\n').count() + 1,
    };
    let table = DeTable::parse(text).map_err(|error| {
        let offset = error.span().map_or(0, |span| span.start);
        ConfigError::at(&origin(offset), error.message().trim())
    })?;
    entries(table.into_inner(), &origin)
}

fn entries(
    table: DeTable<'_>,
    origin: &dyn Fn(usize) -> Origin,
) -> Result<BTreeMap<String, Node>, ConfigError> {
    let mut entries = BTreeMap::new();
    for (key, value) in table {
        // A table from a header like `[storage.lab]` is placed where its key is written.
        let at = key.span().start;
        entries.insert(key.into_inner().into_owned(), node(value, at, origin)?);
    }
    Ok(entries)
}

fn node(
    value: Spanned<DeValue<'_>>,
    key: usize,
    origin: &dyn Fn(usize) -> Origin,
) -> Result<Node, ConfigError> {
    let span = value.span();
    let at = if span.is_empty() { key } else { span.start };
    let value = match value.into_inner() {
        DeValue::String(text) => Value::Text(text.into_owned()),
        DeValue::Integer(integer) => {
            let number = i64::from_str_radix(integer.as_str(), integer.radix())
                .map_err(|_| ConfigError::at(&origin(at), "the number is too large"))?;
            Value::Integer(number)
        }
        DeValue::Float(float) => {
            let number = float
                .as_str()
                .parse()
                .map_err(|_| ConfigError::at(&origin(at), "the number cannot be read"))?;
            Value::Float(number)
        }
        DeValue::Boolean(flag) => Value::Boolean(flag),
        DeValue::Datetime(_) => Value::Datetime,
        DeValue::Array(array) => {
            let items = array.iter().cloned().map(|item| {
                let start = item.span().start;
                node(item, start, origin)
            });
            Value::Array(items.collect::<Result<_, _>>()?)
        }
        DeValue::Table(table) => Value::Table(entries(table, origin)?),
    };
    Ok(Node {
        value,
        origin: origin(at),
    })
}
