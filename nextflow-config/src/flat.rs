//! The flat form of a config, as `nextflow config -flat` prints it: one `a.b.c = value` line
//! per setting.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::value::Value;

/// Every setting that is not a map, in tree order.
pub fn flat(tree: &Value) -> String {
    let mut result = String::new();
    flatten(tree, &mut Vec::new(), &mut result);
    result
}

fn flatten(tree: &Value, path: &mut Vec<String>, result: &mut String) {
    let Value::Map(map) = tree else {
        return;
    };
    let entries: Vec<(Value, Value)> = map
        .borrow()
        .entries
        .iter()
        .map(|(key, value)| (key.0.clone(), value.clone()))
        .collect();
    for (key, value) in entries {
        path.push(identifier_or_quoted(&key.to_string()));
        if let Value::Map(_) = value {
            flatten(&value, path, result);
        } else {
            result.push_str(&format!("{} = {}\n", path.join("."), render(&value)));
        }
        path.pop();
    }
}

/// Nextflow's `ConfigHelper.render0`.
fn render(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::GString(text) => format!("'{text}'"),
        Value::MemoryUnit(_) | Value::Duration(_) => format!("'{value}'"),
        Value::List(list) => {
            let items: Vec<String> = list.borrow().items.iter().map(render).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Set(set) => {
            let items: Vec<String> = set
                .borrow()
                .items
                .iter()
                .map(|key| render(&key.0))
                .collect();
            format!("[{}]", items.join(", "))
        }
        Value::Map(map) => {
            let entries: Vec<String> = map
                .borrow()
                .entries
                .iter()
                .map(|(key, value)| {
                    format!("{}:{}", selector_key(&key.0.to_string()), render(value))
                })
                .collect();
            format!("[{}]", entries.join(", "))
        }
        Value::String(text) => format!("'{}'", escape(text)),
        Value::Closure(closure) => {
            let source = closure.module.source.borrow().clone();
            match source {
                Some(source) => closure.code.span.text(&source).to_owned(),
                None => "{ ... }".into(),
            }
        }
        other => other.to_string(),
    }
}

/// Groovy's `InvokerHelper` escaping of a string it shows in quotes.
fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
        .replace('\u{c}', "\\f")
        .replace('\'', "\\'")
}

/// A map key: a selector keeps its prefix outside the quotes.
fn selector_key(key: &str) -> String {
    for prefix in ["withLabel:", "withName:"] {
        if let Some(rest) = key.strip_prefix(prefix) {
            return format!("{prefix}{}", identifier_or_quoted(rest));
        }
    }
    identifier_or_quoted(key)
}

/// The key as it is when it is a Java identifier, else in single quotes.
fn identifier_or_quoted(key: &str) -> String {
    let mut characters = key.chars();
    let valid = characters
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        && characters.all(|c| c.is_alphanumeric() || c == '_' || c == '$');
    if valid {
        key.to_owned()
    } else {
        format!("'{key}'")
    }
}
