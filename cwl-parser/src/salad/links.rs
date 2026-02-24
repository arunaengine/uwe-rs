//! Link checks after resolution, which also resolve scoped references such as `source: x`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;

use crate::error::Error;
use crate::iri::{Parts, split_fragment};
use crate::node::{Entry, Location, Node, Value};
use crate::salad::context;
use crate::source::Documents;

/// Checks every link below the node, as Salad's `validate_links` does.
/// Errors below `default`, `hints`, `specs` and extension fields are ignored, as Salad only warns.
pub fn check(
    node: &mut Node,
    base: &str,
    ids: &HashSet<String>,
    documents: &Documents,
) -> Result<(), Error> {
    let Node { value, .. } = node;
    match value {
        Value::Map(entries) => {
            let own = context::IDENTIFIERS.iter().find_map(|name| {
                let entry = entries.iter().find(|entry| entry.key == *name)?;
                entry.value.as_str().map(str::to_owned)
            });
            let scope = own.unwrap_or_else(|| base.to_owned());
            for entry in entries.iter_mut() {
                let field = entry.key.as_str();
                // Salad only warns about `$schemas`, which point at ontologies to fetch.
                if context::is_link(field) && !context::is_identity(field) && field != "$schemas" {
                    check_field(entry, &scope, ids, documents)?;
                }
            }
            for entry in entries.iter_mut() {
                let checked = check(&mut entry.value, &scope, ids, documents);
                let tolerated = context::skips_check(&entry.key) || entry.key.contains(':');
                if checked.is_err() && !tolerated {
                    return checked;
                }
            }
            Ok(())
        }
        Value::List(items) => items
            .iter_mut()
            .try_for_each(|item| check(item, base, ids, documents)),
        _ => Ok(()),
    }
}

fn check_field(
    entry: &mut Entry,
    scope: &str,
    ids: &HashSet<String>,
    documents: &Documents,
) -> Result<(), Error> {
    let field = entry.key.as_str();
    if context::skips_check(field) {
        return Ok(());
    }
    let node = &mut entry.value;
    let at = node.location.clone();
    match &mut node.value {
        Value::String(link) => *link = check_link(field, link, scope, ids, documents, &at)?,
        Value::List(items) => {
            for item in items {
                let at = item.location.clone();
                match &mut item.value {
                    Value::String(link) => {
                        *link = check_link(field, link, scope, ids, documents, &at)?;
                    }
                    Value::Map(_) | Value::Null => {}
                    _ => return Err(wrong_kind(field, item)),
                }
            }
        }
        Value::Map(_) | Value::Null => {}
        _ => return Err(wrong_kind(field, node)),
    }
    Ok(())
}

fn wrong_kind(field: &str, node: &Node) -> Error {
    let message = format!(
        "`{field}` must be a string, a list or a map, not {}",
        node.kind()
    );
    Error::new(&node.location, message)
}

/// The resolved form of one link, or an error at its location.
fn check_link(
    field: &str,
    link: &str,
    scope: &str,
    ids: &HashSet<String>,
    documents: &Documents,
    at: &Location,
) -> Result<String, Error> {
    let known = ids.contains(link) || context::term_of(link).is_some();
    if known || (context::is_vocabulary(field) && context::is_term(link)) {
        return Ok(link.to_owned());
    }
    if let Some(levels) = context::reference_scope(field) {
        let found = search_scopes(link, scope, levels, ids);
        return found
            .ok_or_else(|| Error::new(at, format!("`{field}` refers to unknown `{link}`")));
    }
    if documents.exists(link) {
        return Ok(link.to_owned());
    }
    let message = format!("`{field}` refers to `{link}`, which does not exist");
    Err(Error::new(at, message))
}

/// Looks for `name` in the scope of `scope` after dropping `levels` scope levels, then in
/// each enclosing scope, as Salad's `validate_scoped` does.
fn search_scopes(name: &str, scope: &str, levels: usize, ids: &HashSet<String>) -> Option<String> {
    let (document, fragment) = split_fragment(scope);
    let parts = Parts::parse(document);
    let mut segments: Vec<&str> = fragment.unwrap_or("").split('/').collect();
    for _ in 0..levels {
        if segments.pop().is_none() {
            break;
        }
    }
    loop {
        segments.push(name);
        let candidate = parts.compose(Some(&segments.join("/")));
        if ids.contains(&candidate) {
            return Some(candidate);
        }
        segments.pop();
        segments.pop()?;
    }
}

#[cfg(test)]
#[path = "links_tests.rs"]
mod tests;
