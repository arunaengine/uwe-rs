//! `$import` and `$include`: splicing other documents into the one being resolved.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::error::Error;
use crate::iri::split_fragment;
use crate::node::{Entry, Location, Node, Value};
use crate::salad::context::{self, Expansion, Namespaces};
use crate::salad::resolve::{Resolver, find, list_maps};

impl Resolver {
    /// Reads and resolves a whole document, as Salad's `fetch` and `resolve_all` do.
    pub fn document(
        &mut self,
        url: &str,
        at: &Location,
        namespaces: &Namespaces,
    ) -> Result<Node, Error> {
        if let Some(done) = self.resolved.get(url) {
            return Ok(done.clone());
        }
        if self.importing.iter().any(|open| open == url) {
            return Err(Error::new(at, format!("`{url}` imports itself")));
        }
        let tree = with_identifier(self.documents.tree(url, at)?, url);
        self.importing.push(url.to_owned());
        let resolved = self.resolve(tree, url, url, namespaces);
        self.importing.pop();
        let resolved = resolved?;
        self.resolved.insert(url.to_owned(), resolved.clone());
        Ok(resolved)
    }

    /// Replaces a map holding `$import` or `$include` with what it refers to.
    pub(super) fn import(
        &mut self,
        node: Node,
        file: &str,
        namespaces: &Namespaces,
    ) -> Result<Node, Error> {
        let Node {
            value: Value::Map(mut entries),
            location,
        } = node
        else {
            return Ok(node);
        };
        let directive = entries
            .iter()
            .position(|entry| matches!(entry.key.as_str(), "$import" | "$include" | "$mixin"))
            .unwrap_or(0);
        let entry = entries.swap_remove(directive);
        if !entries.is_empty() {
            let message = format!("`{}` must be the only field of its map", entry.key);
            return Err(Error::new(&entry.location, message));
        }
        let at = &entry.value.location;
        let Some(reference) = entry.value.as_str() else {
            let message = format!("`{}` needs a string, not {}", entry.key, entry.value.kind());
            return Err(Error::new(at, message));
        };
        let url = namespaces.expand(reference, file, Expansion::LINK);
        let (document, fragment) = split_fragment(&url);
        match entry.key.as_str() {
            "$include" => {
                let text = self.documents.text(document, at)?;
                Ok(Node::new(Value::String(text.to_string()), location))
            }
            "$import" if document == file && fragment.is_some() => {
                let raw = self.documents.tree(file, at)?;
                match find_raw(&raw, file, &url, namespaces) {
                    Some((object, base)) => self.resolve(object, &base, file, namespaces),
                    None => Err(Error::new(at, format!("`{url}` is not defined"))),
                }
            }
            "$import" => {
                let resolved = self.document(document, at, namespaces)?;
                if fragment.is_none() {
                    return Ok(graph_of(resolved));
                }
                match find_object(&resolved, &url) {
                    Some(object) => Ok(object.clone()),
                    None => Err(Error::new(at, format!("`{url}` is not defined"))),
                }
            }
            key => Err(Error::new(at, format!("`{key}` is not supported"))),
        }
    }
}

/// Gives a map without identifier the document URL as `id`, as Salad's `fetch` does.
fn with_identifier(tree: Node, url: &str) -> Node {
    let Node { value, location } = tree;
    match value {
        Value::Map(mut entries) => {
            let named = context::IDENTIFIERS
                .iter()
                .any(|name| find(&entries, name).is_some());
            if !named {
                let value = Node::new(Value::String(url.to_owned()), location.clone());
                entries.push(Entry {
                    key: "id".to_owned(),
                    location: location.clone(),
                    value,
                });
            }
            Node::new(Value::Map(entries), location)
        }
        value => Node::new(value, location),
    }
}

/// The `$graph` list of a packed document, or the document itself.
fn graph_of(node: Node) -> Node {
    match &node.value {
        Value::Map(entries) => match find(entries, "$graph") {
            Some(graph) => graph.clone(),
            None => node,
        },
        _ => node,
    }
}

/// Finds an object of a document before resolution by the identifier it will get, together
/// with the scope it will be resolved in; identifier maps are read as lists on the way.
fn find_raw(node: &Node, base: &str, id: &str, namespaces: &Namespaces) -> Option<(Node, String)> {
    match &node.value {
        Value::Map(entries) => {
            let own = context::IDENTIFIERS.iter().find_map(|name| {
                let text = find(entries, name)?.as_str()?;
                Some(namespaces.expand(text, base, Expansion::IDENTIFIER))
            });
            if own.as_deref() == Some(id) {
                return Some((node.clone(), base.to_owned()));
            }
            let scope = own.unwrap_or_else(|| base.to_owned());
            let mut entries = entries.clone();
            list_maps(&mut entries).ok()?;
            entries.iter().find_map(|entry| {
                let inner = match entry.key.as_str() {
                    "run" => format!("{scope}/run"),
                    _ => scope.clone(),
                };
                find_raw(&entry.value, &inner, id, namespaces)
            })
        }
        Value::List(items) => items
            .iter()
            .find_map(|item| find_raw(item, base, id, namespaces)),
        _ => None,
    }
}

/// The object whose resolved identifier is the given IRI.
pub fn find_object<'n>(node: &'n Node, id: &str) -> Option<&'n Node> {
    match &node.value {
        Value::Map(entries) => {
            let named = context::IDENTIFIERS
                .iter()
                .any(|name| find(entries, name).and_then(Node::as_str) == Some(id));
            if named {
                return Some(node);
            }
            entries
                .iter()
                .find_map(|entry| find_object(&entry.value, id))
        }
        Value::List(items) => items.iter().find_map(|item| find_object(item, id)),
        _ => None,
    }
}

#[cfg(test)]
#[path = "import_tests.rs"]
mod tests;
