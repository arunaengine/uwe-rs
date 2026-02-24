//! Reads records of the typed model from resolved nodes, with errors at the right location.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::error::Error;
use crate::iri::Parts;
use crate::model::expression::Expression;
use crate::model::process::Version;
use crate::node::{Entry, Location, Node, Value};

/// What reading needs besides the node: the version of the document.
pub struct Context {
    pub version: Version,
}

impl Context {
    /// Fails when the document's version predates the feature.
    pub fn require(&self, since: Version, feature: &str, at: &Location) -> Result<(), Error> {
        if self.version.release() >= since {
            return Ok(());
        }
        let message = format!(
            "{feature} needs cwlVersion {since} or later, not {}",
            self.version
        );
        Err(Error::new(at, message))
    }

    /// Reads a string that may contain expressions.
    pub fn expression(&self, node: &Node) -> Result<Expression, Error> {
        let text = text(node)?;
        Expression::parse(&text, self.version.old_escapes())
            .map_err(|message| Error::new(&node.location, message))
    }

    /// Reads a string that must be an expression, the `Expression` type of the schema.
    pub fn code(&self, node: &Node) -> Result<Expression, Error> {
        let expression = self.expression(node)?;
        if !expression.text.contains("$(") && !expression.text.contains("${") {
            let message = format!("`{}` is not an expression in $() or ${{}}", expression.text);
            return Err(Error::new(&node.location, message));
        }
        Ok(expression)
    }
}

/// The fields of one map, read one by one so that unknown fields can be reported.
pub struct Record<'n> {
    node: &'n Node,
    entries: &'n [Entry],
    used: Vec<bool>,
    what: &'static str,
}

impl<'n> Record<'n> {
    /// Starts reading a map; `what` names the record in error messages.
    pub fn new(node: &'n Node, what: &'static str) -> Result<Self, Error> {
        let Some(entries) = node.as_map() else {
            let message = format!("a {what} must be a map, not {}", node.kind());
            return Err(Error::new(&node.location, message));
        };
        Ok(Self {
            node,
            entries,
            used: vec![false; entries.len()],
            what,
        })
    }

    pub fn location(&self) -> &'n Location {
        &self.node.location
    }

    /// The value of a field; null counts as absent, as it does for optional fields.
    pub fn optional(&mut self, name: &str) -> Option<&'n Node> {
        let index = self.entries.iter().position(|entry| entry.key == name)?;
        self.used[index] = true;
        let value = &self.entries[index].value;
        (value.value != Value::Null).then_some(value)
    }

    pub fn required(&mut self, name: &str) -> Result<&'n Node, Error> {
        match self.optional(name) {
            Some(value) => Ok(value),
            None => {
                let message = format!("the {} has no `{name}` field", self.what);
                Err(Error::new(&self.node.location, message))
            }
        }
    }

    /// A field that exists only from the given version on.
    pub fn since(
        &mut self,
        name: &str,
        version: Version,
        context: &Context,
    ) -> Result<Option<&'n Node>, Error> {
        let value = self.optional(name);
        if let Some(value) = value {
            context.require(version, &format!("`{name}`"), &value.location)?;
        }
        Ok(value)
    }

    /// Ends reading: unknown fields are errors, and fields named by an IRI are extensions.
    /// Identifier fields and `$` directives are allowed everywhere, as in Salad's validation.
    pub fn finish(self) -> Result<Vec<Entry>, Error> {
        let mut extensions = Vec::new();
        for (entry, used) in self.entries.iter().zip(&self.used) {
            let key = entry.key.as_str();
            if *used || matches!(key, "id" | "name") || key.starts_with(['$', '@']) {
                continue;
            }
            if Parts::parse(key).scheme.is_some() {
                extensions.push(entry.clone());
                continue;
            }
            let message = format!("the {} has no field `{key}`", self.what);
            return Err(Error::new(&entry.location, message));
        }
        Ok(extensions)
    }
}

fn wrong_kind(node: &Node, expected: &str) -> Error {
    let message = format!("expected {expected}, not {}", node.kind());
    Error::new(&node.location, message)
}

pub fn text(node: &Node) -> Result<String, Error> {
    node.as_str()
        .map(str::to_owned)
        .ok_or_else(|| wrong_kind(node, "a string"))
}

pub fn boolean(node: &Node) -> Result<bool, Error> {
    match node.value {
        Value::Bool(value) => Ok(value),
        _ => Err(wrong_kind(node, "a boolean")),
    }
}

pub fn integer(node: &Node) -> Result<i64, Error> {
    match node.value {
        Value::Integer(value) => Ok(value),
        _ => Err(wrong_kind(node, "an integer")),
    }
}

/// The items of a list; a single value counts as a list of one where the schema allows both.
pub fn items(node: &Node) -> &[Node] {
    match &node.value {
        Value::List(items) => items,
        _ => std::slice::from_ref(node),
    }
}

pub fn list(node: &Node) -> Result<&[Node], Error> {
    node.as_list().ok_or_else(|| wrong_kind(node, "a list"))
}

/// A string or a list of strings.
pub fn texts(node: &Node) -> Result<Vec<String>, Error> {
    items(node).iter().map(text).collect()
}

/// The `doc` of a record: a string or a list of strings.
pub fn documentation(record: &mut Record<'_>) -> Result<Vec<String>, Error> {
    record.optional("doc").map(texts).unwrap_or(Ok(Vec::new()))
}

/// One of the symbols of an enum field, which a vocabulary IRI may also name.
pub fn symbol<T: Copy>(node: &Node, symbols: &[(&str, T)]) -> Result<T, Error> {
    let found = node.as_str().and_then(|text| {
        let short = text.rsplit(['#', '/']).next().unwrap_or(text);
        symbols.iter().find(|(name, _)| *name == short)
    });
    found.map(|(_, value)| *value).ok_or_else(|| {
        let names: Vec<&str> = symbols.iter().map(|(name, _)| *name).collect();
        wrong_kind(node, &format!("one of {}", names.join(", ")))
    })
}
