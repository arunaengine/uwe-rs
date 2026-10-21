//! Substitutes simple tags and evaluates expression tags in Argo text.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::OnceCell;
use std::collections::BTreeMap;

use argo_parser::{Node, Value as Yaml};

use crate::expr;
use crate::value::Value;

/// The variables of one substitution phase: flat names like `inputs.parameters.x` with text
/// values, and the expression environment built from them on first use.
#[derive(Clone, Debug, Default)]
pub struct Variables {
    values: BTreeMap<String, String>,
    /// Values that only expressions see, like artifacts for `fromExpression`.
    objects: BTreeMap<String, Value>,
    environment: OnceCell<BTreeMap<String, Value>>,
}

impl Variables {
    pub fn new(values: BTreeMap<String, String>) -> Self {
        Variables {
            values,
            objects: BTreeMap::new(),
            environment: OnceCell::new(),
        }
    }

    /// Adds a value that expressions see under a dotted name.
    pub fn with_object(mut self, name: impl Into<String>, value: Value) -> Self {
        self.objects.insert(name.into(), value);
        self.environment = OnceCell::new();
        self
    }

    /// Adds values, which replace values of the same name.
    pub fn with_values(mut self, values: BTreeMap<String, String>) -> Self {
        self.values.extend(values);
        self.environment = OnceCell::new();
        self
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.values.contains_key(name) || self.objects.contains_key(name)
    }

    /// The expression environment: dotted names nested into maps, as Argo's `expand.Expand`
    /// does, which drops a name when a longer name continues it (`item` beside `item.a`).
    pub fn environment(&self) -> &BTreeMap<String, Value> {
        self.environment.get_or_init(|| {
            let mut environment = BTreeMap::new();
            let names: Vec<&String> = self.values.keys().chain(self.objects.keys()).collect();
            for name in &names {
                let prefix = format!("{name}.");
                if names.iter().any(|other| other.starts_with(&prefix)) {
                    continue;
                }
                let value = match self.objects.get(*name) {
                    Some(object) => object.clone(),
                    None => Value::String(self.values[*name].clone()),
                };
                insert_path(&mut environment, name, value);
            }
            environment
        })
    }
}

fn insert_path(environment: &mut BTreeMap<String, Value>, name: &str, value: Value) {
    let mut parts = name.split('.').peekable();
    let mut map = environment;
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            map.insert(part.to_owned(), value);
            return;
        }
        let entry = map
            .entry(part.to_owned())
            .or_insert_with(|| Value::Map(BTreeMap::new()));
        if !matches!(entry, Value::Map(_)) {
            *entry = Value::Map(BTreeMap::new());
        }
        let Value::Map(inner) = entry else {
            return;
        };
        map = inner;
    }
}

/// Replaces the tags of `text`. With `allow_unresolved`, a tag that cannot be resolved yet stays
/// as it is; otherwise it is an error.
pub fn replace(
    text: &str,
    variables: &Variables,
    allow_unresolved: bool,
) -> Result<String, String> {
    if !text.contains("{{") {
        return Ok(text.to_owned());
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    // Like fasttemplate: a tag runs from `{{` to the next `}}`; without one the rest is text.
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str("{{");
            rest = after;
            break;
        };
        let tag = &after[..end];
        rest = &after[end + 2..];
        match tag.strip_prefix('=') {
            Some(expression) => out.push_str(&replace_expression(
                expression,
                variables,
                allow_unresolved,
            )?),
            None => out.push_str(&replace_simple(tag, variables, allow_unresolved)?),
        }
    }
    out.push_str(rest);
    Ok(out)
}

fn replace_simple(
    tag: &str,
    variables: &Variables,
    allow_unresolved: bool,
) -> Result<String, String> {
    if let Some(value) = variables.get(tag.trim()) {
        return Ok(value.to_owned());
    }
    // A tag like `{{a{{b}}` holds a nested tag, which resolves on its own.
    if let Some(index) = tag.rfind("{{")
        && index > 0
        && let Some(value) = variables.get(&tag[index + 2..])
    {
        return Ok(format!("{{{{{}{value}", &tag[..index]));
    }
    if allow_unresolved {
        Ok(format!("{{{{{tag}}}}}"))
    } else {
        Err(format!("failed to resolve {{{{{tag}}}}}"))
    }
}

fn replace_expression(
    expression: &str,
    variables: &Variables,
    allow_unresolved: bool,
) -> Result<String, String> {
    let unresolved = || format!("{{{{={expression}}}}}");
    if allow_unresolved {
        // Variables that only exist later: loop items, retry counts and the status in exit
        // handlers.
        let names = expr::identifiers(expression);
        let late = ["item", "retries"]
            .iter()
            .any(|late| !variables.contains(late) && names.iter().any(|name| name == late))
            || (expression.contains("workflow.status") && !variables.contains("workflow.status"))
            || (expression.contains("workflow.failures")
                && !variables.contains("workflow.failures"));
        if late {
            return Ok(unresolved());
        }
    }
    match expr::evaluate(expression, variables.environment()) {
        Ok(Value::Nil) if allow_unresolved => Ok(unresolved()),
        Ok(Value::Nil) => Err(format!("failed to evaluate expression {expression:?}")),
        // Argo v4 writes the result as JSON, without the quotes of a string.
        Ok(Value::String(text)) => Ok(text),
        Ok(value) => Ok(value.to_json()),
        Err(_) if allow_unresolved => Ok(unresolved()),
        Err(error) => Err(format!("failed to evaluate expression: {error}")),
    }
}

/// Replaces the tags in every string and mapping key of a value tree.
pub fn replace_node(
    node: &mut Node,
    variables: &Variables,
    allow_unresolved: bool,
) -> Result<(), String> {
    match &mut node.value {
        Yaml::String(text) => {
            if text.contains("{{") {
                *text = replace(text, variables, allow_unresolved)?;
            }
        }
        Yaml::Sequence(items) => {
            for item in items {
                replace_node(item, variables, allow_unresolved)?;
            }
        }
        Yaml::Mapping(entries) => {
            for entry in entries {
                if entry.key.contains("{{") {
                    entry.key = replace(&entry.key, variables, allow_unresolved)?;
                }
                replace_node(&mut entry.value, variables, allow_unresolved)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Whether a text still holds a tag.
pub fn has_tag(text: &str) -> bool {
    text.find("{{")
        .is_some_and(|start| text[start + 2..].contains("}}"))
}

/// The text of the first tag in a text, without its braces.
pub fn first_tag(text: &str) -> Option<&str> {
    let after = &text[text.find("{{")? + 2..];
    Some(&after[..after.find("}}")?])
}

#[cfg(test)]
#[path = "tags_tests.rs"]
mod tests;
