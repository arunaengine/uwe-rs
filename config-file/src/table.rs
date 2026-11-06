//! A table of the config file, like `[storage.lab]`, and its settings by type. A value of the
//! wrong type, an unknown key or a missing key is an error that names the file and the line.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use crate::node::{Node, Value};
use crate::{ConfigError, Origin, Secret};

/// A table of settings and its full name, like `storage.lab`.
#[derive(Clone, Debug)]
pub struct Table<'a> {
    name: String,
    entries: &'a BTreeMap<String, Node>,
    origin: &'a Origin,
}

/// The key that names the environment variable of a secret.
const ENVIRONMENT: &str = "environment";

impl<'a> Table<'a> {
    pub(crate) fn root(entries: &'a BTreeMap<String, Node>, origin: &'a Origin) -> Table<'a> {
        Table {
            name: String::new(),
            entries,
            origin,
        }
    }

    /// The full name of the table, like `storage.lab`; empty for the top level.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Where the table was first written.
    pub fn origin(&self) -> &Origin {
        self.origin
    }

    /// Where the effective value was written, including overrides from later files.
    pub fn value_origin(&self, key: &str) -> Option<&Origin> {
        self.get(key).map(|node| &node.origin)
    }

    /// The keys of the table, in order.
    pub fn keys(&self) -> impl Iterator<Item = &'a str> + use<'a> {
        self.entries.keys().map(String::as_str)
    }

    /// Checks that every key is one of `known`.
    pub fn only(&self, known: &[&str]) -> Result<(), ConfigError> {
        let Some((key, node)) = self
            .entries
            .iter()
            .find(|(key, _)| !known.contains(&key.as_str()))
        else {
            return Ok(());
        };
        let message = match self.name.is_empty() {
            true => format!("uwe has no section `{key}`; it knows {}", list(known)),
            false => format!(
                "`[{}]` has no setting `{key}`; it knows {}",
                self.name,
                list(known)
            ),
        };
        Err(ConfigError::at(&node.origin, message))
    }

    /// The table `key`, or `None` without one.
    pub fn table(&self, key: &str) -> Result<Option<Table<'a>>, ConfigError> {
        self.get(key)
            .map(|node| self.as_table(key, node))
            .transpose()
    }

    /// Every entry of this table, each of which must be a table, like the providers of
    /// `[storage]`.
    pub fn tables(&self) -> Result<Vec<Table<'a>>, ConfigError> {
        (self.entries.iter())
            .map(|(key, node)| self.as_table(key, node))
            .collect()
    }

    /// The text `key`.
    pub fn text(&self, key: &str) -> Result<Option<String>, ConfigError> {
        let Some(node) = self.get(key) else {
            return Ok(None);
        };
        match &node.value {
            Value::Text(text) => Ok(Some(text.clone())),
            other => Err(self.wrong(key, node, "text", other)),
        }
    }

    /// The texts `key`: a list of texts, or one text.
    pub fn texts(&self, key: &str) -> Result<Option<Vec<String>>, ConfigError> {
        let Some(node) = self.get(key) else {
            return Ok(None);
        };
        match &node.value {
            Value::Text(text) => Ok(Some(vec![text.clone()])),
            Value::Array(items) => (items.iter())
                .map(|item| match &item.value {
                    Value::Text(text) => Ok(text.clone()),
                    other => Err(self.wrong(key, item, "a list of texts", other)),
                })
                .collect::<Result<_, _>>()
                .map(Some),
            other => Err(self.wrong(key, node, "a list of texts", other)),
        }
    }

    /// The flag `key`: `true` or `false`.
    pub fn flag(&self, key: &str) -> Result<Option<bool>, ConfigError> {
        let Some(node) = self.get(key) else {
            return Ok(None);
        };
        match &node.value {
            Value::Boolean(flag) => Ok(Some(*flag)),
            other => Err(self.wrong(key, node, "true or false", other)),
        }
    }

    /// The whole number `key`.
    pub fn integer(&self, key: &str) -> Result<Option<i64>, ConfigError> {
        let Some(node) = self.get(key) else {
            return Ok(None);
        };
        match &node.value {
            Value::Integer(number) => Ok(Some(*number)),
            other => Err(self.wrong(key, node, "a whole number", other)),
        }
    }

    /// The number `key`, whole or not.
    pub fn number(&self, key: &str) -> Result<Option<f64>, ConfigError> {
        let Some(node) = self.get(key) else {
            return Ok(None);
        };
        match &node.value {
            Value::Integer(number) => Ok(Some(*number as f64)),
            Value::Float(number) => Ok(Some(*number)),
            other => Err(self.wrong(key, node, "a number", other)),
        }
    }

    /// The value `key` as JSON, for settings of any shape, like a list of tables. A date is an
    /// error, as JSON has none.
    pub fn json(&self, key: &str) -> Result<Option<serde_json::Value>, ConfigError> {
        self.get(key)
            .map(|node| self.as_json(key, node))
            .transpose()
    }

    /// The secret `key`: the secret as text, or `{ environment = "NAME" }` for the name of the
    /// environment variable that holds it.
    pub fn secret(&self, key: &str) -> Result<Option<Secret>, ConfigError> {
        let Some(node) = self.get(key) else {
            return Ok(None);
        };
        let origin = node.origin.clone();
        match &node.value {
            Value::Text(text) => Ok(Some(Secret::written(text.clone(), origin))),
            Value::Table(entries) => {
                let reference = self.as_table(key, node)?;
                reference.only(&[ENVIRONMENT])?;
                match entries.get(ENVIRONMENT).map(|name| &name.value) {
                    Some(Value::Text(name)) if !name.is_empty() => {
                        Ok(Some(Secret::environment(name.clone(), origin)))
                    }
                    _ => Err(ConfigError::at(
                        &origin,
                        format!(
                            "`{}` needs the name of an environment variable, like \
                             {key} = {{ environment = \"NAME\" }}",
                            self.full(key)
                        ),
                    )),
                }
            }
            other => Err(self.wrong(key, node, "text or { environment = \"NAME\" }", other)),
        }
    }

    /// An error about the value of `key`, at its line; at the table's line when it is missing.
    pub fn invalid(&self, key: &str, message: impl std::fmt::Display) -> ConfigError {
        let origin = self.get(key).map_or(self.origin, |node| &node.origin);
        ConfigError::at(origin, format!("`{}` {message}", self.full(key)))
    }

    /// An error for a table without `key`, which it needs, at the table's line.
    pub fn missing(&self, key: &str, hint: impl std::fmt::Display) -> ConfigError {
        let message = format!("`[{}]` needs `{key}`, {hint}", self.name);
        ConfigError::at(self.origin, message)
    }

    fn get(&self, key: &str) -> Option<&'a Node> {
        self.entries.get(key)
    }

    fn full(&self, key: &str) -> String {
        match self.name.is_empty() {
            true => key.to_owned(),
            false => format!("{}.{key}", self.name),
        }
    }

    fn as_table(&self, key: &str, node: &'a Node) -> Result<Table<'a>, ConfigError> {
        match &node.value {
            Value::Table(entries) => Ok(Table {
                name: self.full(key),
                entries,
                origin: &node.origin,
            }),
            other => Err(self.wrong(key, node, "a table", other)),
        }
    }

    fn as_json(&self, key: &str, node: &Node) -> Result<serde_json::Value, ConfigError> {
        use serde_json::Value as Json;
        Ok(match &node.value {
            Value::Text(text) => Json::String(text.clone()),
            Value::Integer(number) => Json::from(*number),
            Value::Float(number) => Json::from(*number),
            Value::Boolean(flag) => Json::Bool(*flag),
            Value::Array(items) => Json::Array(
                (items.iter())
                    .map(|item| self.as_json(key, item))
                    .collect::<Result<_, _>>()?,
            ),
            Value::Table(entries) => Json::Object(
                (entries.iter())
                    .map(|(name, entry)| Ok((name.clone(), self.as_json(key, entry)?)))
                    .collect::<Result<_, ConfigError>>()?,
            ),
            other @ Value::Datetime => {
                let wanted = "text, a number, a flag, a list or a table";
                return Err(self.wrong(key, node, wanted, other));
            }
        })
    }

    fn wrong(&self, key: &str, node: &Node, wanted: &str, found: &Value) -> ConfigError {
        let message = format!(
            "`{}` must be {wanted}, not {}",
            self.full(key),
            found.kind()
        );
        ConfigError::at(&node.origin, message)
    }
}

/// Names in backticks, joined with commas and "and".
fn list(names: &[&str]) -> String {
    let quoted: Vec<String> = names.iter().map(|name| format!("`{name}`")).collect();
    match quoted.as_slice() {
        [] => "nothing".to_owned(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}
