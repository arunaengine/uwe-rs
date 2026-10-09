//! The Schema Salad walk over a document: maps to lists, type DSL, identifiers and links.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{HashMap, HashSet};

use crate::error::Error;
use crate::node::{Entry, Location, Node, Value};
use crate::salad::context::{self, Expansion, Namespaces};
use crate::source::Documents;

/// Resolves documents as Schema Salad's `resolve_all` does, and indexes every identifier.
/// The index spans all documents of one load, as the single loader of cwltool does.
pub struct Resolver {
    pub documents: Documents,
    pub ids: HashSet<String>,
    pub(super) resolved: HashMap<String, Node>,
    pub(super) importing: Vec<String>,
    /// Whether the CWL v1.0 context applies: no secondary files DSL, no `run` subscope and
    /// no links in `specs`.
    pub legacy: bool,
}

impl Resolver {
    pub fn new(documents: Documents) -> Self {
        Self {
            documents,
            ids: HashSet::new(),
            resolved: HashMap::new(),
            importing: Vec::new(),
            legacy: false,
        }
    }

    /// Resolves one node; `base` is the identifier scope and `file` the URL of its document.
    pub fn resolve(
        &mut self,
        node: Node,
        base: &str,
        file: &str,
        namespaces: &Namespaces,
    ) -> Result<Node, Error> {
        let Node { value, location } = node;
        match value {
            Value::Map(entries) => self.resolve_map(entries, location, base, file, namespaces),
            Value::List(items) => {
                let items = self.resolve_list(items, base, file, namespaces)?;
                Ok(Node::new(Value::List(items), location))
            }
            value => Ok(Node::new(value, location)),
        }
    }

    fn resolve_map(
        &mut self,
        entries: Vec<Entry>,
        location: Location,
        base: &str,
        file: &str,
        namespaces: &Namespaces,
    ) -> Result<Node, Error> {
        if has_directive(&entries) {
            return self.import(Node::new(Value::Map(entries), location), file, namespaces);
        }
        let mut entries = entries;
        let mut base = base.to_owned();
        let mut scope = namespaces.clone();
        if let Some(new_base) = entry_text(&entries, "$base") {
            base = new_base.to_owned();
        }
        if let Some(declared) = find(&entries, "$namespaces") {
            scope = namespaces.extend(declarations(declared)?);
        }
        for entry in &mut entries {
            if context::is_identity(&entry.key)
                && let Value::String(text) = &mut entry.value.value
            {
                *text = scope.expand(text, &base, Expansion::IDENTIFIER);
                self.ids.insert(text.clone());
            }
        }
        if let Some(index) = entries.iter().position(|entry| entry.key == "$graph") {
            let graph = entries[index].value.clone();
            entries[index].value = self.resolve(graph, &base, file, &scope)?;
            return Ok(Node::new(Value::Map(entries), location));
        }
        let mut entries = normalize_keys(entries, &scope);
        list_maps(&mut entries)?;
        apply_dsl(&mut entries, self.legacy);
        for name in context::IDENTIFIERS {
            let Some(entry) = entries.iter_mut().find(|entry| entry.key == name) else {
                continue;
            };
            let Value::String(text) = &mut entry.value.value else {
                let message = format!("the identifier field `{name}` must be a string");
                return Err(Error::new(&entry.value.location, message));
            };
            *text = scope.expand(text, &base, Expansion::IDENTIFIER);
            self.ids.insert(text.clone());
            base = text.clone();
        }
        for entry in &mut entries {
            if !context::is_link(&entry.key) || (self.legacy && entry.key == "specs") {
                continue;
            }
            let identity = context::is_identity(&entry.key);
            let rule = Expansion::field(&entry.key);
            let mut expand = |text: &mut String| {
                if entry.key == "class" && text == "ShmSize" {
                    *text = "http://commonwl.org/cwltool#ShmSize".into();
                }
                if identity {
                    *text = scope.expand(text, &base, Expansion::IDENTIFIER);
                    self.ids.insert(text.clone());
                }
                *text = scope.expand(text, &base, rule);
            };
            match &mut entry.value.value {
                Value::String(text) if !identity => expand(text),
                Value::List(items) => {
                    for item in items {
                        if let Value::String(text) = &mut item.value {
                            expand(text);
                        }
                    }
                }
                _ => {}
            }
        }
        let mut resolved = Vec::with_capacity(entries.len());
        for entry in entries {
            let scoped = if entry.key == "run" && !self.legacy {
                format!("{base}/run")
            } else {
                base.clone()
            };
            let value = self.resolve(entry.value, &scoped, file, &scope)?;
            resolved.push(Entry { value, ..entry });
        }
        Ok(Node::new(Value::Map(resolved), location))
    }

    fn resolve_list(
        &mut self,
        items: Vec<Node>,
        base: &str,
        file: &str,
        namespaces: &Namespaces,
    ) -> Result<Vec<Node>, Error> {
        let mut resolved = Vec::with_capacity(items.len());
        for item in items {
            let imports =
                matches!(&item.value, Value::Map(entries) if find(entries, "$import").is_some());
            let item = self.resolve(item, base, file, namespaces)?;
            match item.value {
                Value::List(spliced) if imports => resolved.extend(spliced),
                value => resolved.push(Node::new(value, item.location)),
            }
        }
        Ok(resolved)
    }
}

fn has_directive(entries: &[Entry]) -> bool {
    entries
        .iter()
        .any(|entry| matches!(entry.key.as_str(), "$import" | "$include" | "$mixin"))
}

pub fn find<'n>(entries: &'n [Entry], key: &str) -> Option<&'n Node> {
    let found = entries.iter().find(|entry| entry.key == key);
    found.map(|entry| &entry.value)
}

fn entry_text<'n>(entries: &'n [Entry], key: &str) -> Option<&'n str> {
    find(entries, key).and_then(Node::as_str)
}

/// The prefix declarations of a `$namespaces` map.
fn declarations(node: &Node) -> Result<Vec<(String, String)>, Error> {
    let Some(entries) = node.as_map() else {
        return Err(Error::new(&node.location, "$namespaces must be a map"));
    };
    let mut pairs = Vec::new();
    for entry in entries {
        let Some(iri) = entry.value.as_str() else {
            let message = format!("the namespace `{}` must be a string", entry.key);
            return Err(Error::new(&entry.value.location, message));
        };
        pairs.push((entry.key.clone(), iri.to_owned()));
    }
    Ok(pairs)
}

/// Rewrites prefixed or full IRI keys, so that `cwl:inputs` becomes `inputs`.
fn normalize_keys(entries: Vec<Entry>, scope: &Namespaces) -> Vec<Entry> {
    let term = Expansion {
        term: true,
        ..Expansion::LINK
    };
    entries
        .into_iter()
        .map(|entry| Entry {
            key: scope.expand(&entry.key, "", term),
            ..entry
        })
        .collect()
}

/// Turns identifier maps such as `inputs: {x: File}` into lists, sorted by key as Salad does.
pub(super) fn list_maps(entries: &mut [Entry]) -> Result<(), Error> {
    for entry in entries {
        let Some(form) = context::map_form(&entry.key) else {
            continue;
        };
        let Value::Map(pairs) = &entry.value.value else {
            continue;
        };
        if has_directive(pairs) {
            continue;
        }
        let mut pairs = pairs.clone();
        pairs.sort_by(|left, right| left.key.cmp(&right.key));
        let mut items = Vec::with_capacity(pairs.len());
        for pair in pairs {
            let subject = Entry {
                key: form.subject.to_owned(),
                location: pair.location.clone(),
                value: Node::new(Value::String(pair.key.clone()), pair.location.clone()),
            };
            let mut fields = match (pair.value.value, form.predicate) {
                (Value::Map(fields), _) => fields,
                (value, Some(predicate)) => vec![Entry {
                    key: predicate.to_owned(),
                    location: pair.location.clone(),
                    value: Node::new(value, pair.value.location),
                }],
                (_, None) => {
                    let message = format!("the value of `{}` must be a map", pair.key);
                    return Err(Error::new(&pair.location, message));
                }
            };
            match fields.iter_mut().find(|field| field.key == form.subject) {
                Some(field) => field.value = subject.value,
                None => fields.push(subject),
            }
            items.push(Node::new(Value::Map(fields), pair.location));
        }
        entry.value.value = Value::List(items);
    }
    Ok(())
}

/// Expands the type DSL (`File[]?`) and the secondary files DSL (`.bai?`).
fn apply_dsl(entries: &mut [Entry], legacy: bool) {
    for entry in entries {
        let expand: fn(&Node) -> Option<Value> = match entry.key.as_str() {
            "type" => type_dsl,
            "secondaryFiles" if !legacy => secondary_dsl,
            _ => continue,
        };
        match &entry.value.value {
            Value::String(_) => {
                if let Some(value) = expand(&entry.value) {
                    entry.value.value = value;
                }
            }
            Value::List(items) => {
                let mut flat: Vec<Node> = Vec::new();
                for item in items {
                    let expanded = match expand(item) {
                        Some(value) => Node::new(value, item.location.clone()),
                        None => item.clone(),
                    };
                    let parts = match expanded.value {
                        Value::List(parts) => parts,
                        value => vec![Node::new(value, expanded.location)],
                    };
                    for part in parts {
                        if !flat.iter().any(|seen| same_value(seen, &part)) {
                            flat.push(part);
                        }
                    }
                }
                entry.value.value = Value::List(flat);
            }
            _ => {}
        }
    }
}

fn type_dsl(node: &Node) -> Option<Value> {
    let text = node.as_str()?;
    let (text, optional) = match text.strip_suffix('?') {
        Some(rest) => (rest, true),
        None => (text, false),
    };
    let at = &node.location;
    let expanded = match text.strip_suffix("[]") {
        Some(items) if items.ends_with("[]") => return None,
        Some(items) => map_value(
            at,
            [("type", text_value("array")), ("items", text_value(items))],
        ),
        None if optional => text_value(text),
        None => return None,
    };
    if !optional {
        return Some(expanded);
    }
    let null = Node::new(text_value("null"), at.clone());
    Some(Value::List(vec![null, Node::new(expanded, at.clone())]))
}

fn secondary_dsl(node: &Node) -> Option<Value> {
    let text = node.as_str()?;
    let (pattern, required) = match text.strip_suffix('?') {
        Some(pattern) => (pattern, Value::Bool(false)),
        None => (text, Value::Null),
    };
    let at = &node.location;
    Some(map_value(
        at,
        [("pattern", text_value(pattern)), ("required", required)],
    ))
}

fn text_value(text: &str) -> Value {
    Value::String(text.to_owned())
}

fn map_value<const N: usize>(at: &Location, fields: [(&str, Value); N]) -> Value {
    let entries = fields
        .into_iter()
        .map(|(key, value)| Entry {
            key: key.to_owned(),
            location: at.clone(),
            value: Node::new(value, at.clone()),
        })
        .collect();
    Value::Map(entries)
}

/// Whether two nodes hold the same value, whatever their locations.
pub fn same_value(left: &Node, right: &Node) -> bool {
    match (&left.value, &right.value) {
        (Value::List(left), Value::List(right)) => {
            left.len() == right.len() && left.iter().zip(right).all(|(a, b)| same_value(a, b))
        }
        (Value::Map(left), Value::Map(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(a, b)| a.key == b.key && same_value(&a.value, &b.value))
        }
        (left, right) => left == right,
    }
}

#[cfg(test)]
#[path = "resolve_tests.rs"]
pub(crate) mod tests;
