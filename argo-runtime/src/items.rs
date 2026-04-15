//! Loops: `withItems`, `withParam` and `withSequence` expand a step or task into one per item
//! (workflow/controller/steps.go, `expandStep` and `processItem`), named like `name(0:item)`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use argo_parser::model::Sequence;
use argo_parser::{Node, Value as Yaml};

use crate::tags::{Variables, replace};
use crate::value::Value;

/// The fields of a step or task that loop.
pub(crate) const LOOP_FIELDS: [&str; 3] = ["withItems", "withParam", "withSequence"];

/// Whether a step or task loops.
pub(crate) fn loops(node: &Node) -> bool {
    LOOP_FIELDS
        .iter()
        .any(|field| match node.get(field).map(|node| &node.value) {
            None | Some(Yaml::Null) => false,
            Some(Yaml::String(text)) => !text.is_empty(),
            Some(_) => true,
        })
}

/// The items of a loop.
///
/// A loop over values that are not known, as a run with empty outputs or placeholder
/// parameters leaves them, has no items, and `warn` hears about it: an empty `withParam`, one
/// that still holds a tag or is a placeholder, and a `withSequence` bound that is empty or
/// still holds a tag. Argo fails on such values; they only occur when data is missing.
pub(crate) fn items(
    node: &Node,
    sequence: Option<&Sequence>,
    warn: &mut dyn FnMut(String),
) -> Result<Vec<Value>, String> {
    if let Some(Node {
        value: Yaml::Sequence(items),
        ..
    }) = node.get("withItems")
    {
        return Ok(items.iter().map(yaml_value).collect());
    }
    if let Some(param) = node.get("withParam") {
        let text = match &param.value {
            Yaml::String(text) => text.clone(),
            _ => crate::pod::node_json(param),
        };
        let unknown = if text.trim().is_empty() {
            Some("is empty")
        } else if crate::tags::has_tag(&text) {
            Some("holds an unresolved tag")
        } else if text.trim().starts_with(crate::run::PLACEHOLDER) {
            Some("is a placeholder")
        } else {
            None
        };
        if let Some(reason) = unknown {
            warn(format!(
                "withParam of {} {reason}; the loop has no items",
                node.get("name").and_then(Node::as_str).unwrap_or("")
            ));
            return Ok(Vec::new());
        }
        return match Value::parse_json(&text) {
            Ok(Value::Array(items)) => Ok(items),
            Ok(_) => Err(format!(
                "withParam value could not be parsed as a JSON list: {}",
                text.trim()
            )),
            Err(error) => Err(format!(
                "withParam value could not be parsed as a JSON list: {}: {error}",
                text.trim()
            )),
        };
    }
    if let Some(sequence) = sequence {
        let bounds = [&sequence.start, &sequence.end, &sequence.count];
        let unknown = bounds.into_iter().flatten().any(|bound| {
            bound.0.trim().is_empty()
                || crate::tags::has_tag(&bound.0)
                || bound.0.trim().starts_with(crate::run::PLACEHOLDER)
        });
        if unknown {
            warn(format!(
                "withSequence of {} has a bound that is not known; the loop has no items",
                node.get("name").and_then(Node::as_str).unwrap_or("")
            ));
            return Ok(Vec::new());
        }
        return expand_sequence(sequence);
    }
    Ok(Vec::new())
}

fn expand_sequence(sequence: &Sequence) -> Result<Vec<Value>, String> {
    let number = |value: &Option<argo_parser::model::AnyString>,
                  field: &str|
     -> Result<Option<i64>, String> {
        match value {
            None => Ok(None),
            Some(text) => text
                .0
                .trim()
                .parse()
                .map(Some)
                .map_err(|_| format!("withSequence.{field} `{}` is not a number", text.0)),
        }
    };
    let start = number(&sequence.start, "start")?.unwrap_or(0);
    let end = match (
        number(&sequence.end, "end")?,
        number(&sequence.count, "count")?,
    ) {
        (Some(end), _) => end,
        (None, Some(0)) => return Ok(Vec::new()),
        (None, Some(count)) => start + count - 1,
        (None, None) => return Err("neither end nor count was specified in withSequence".into()),
    };
    let format = if sequence.format.is_empty() {
        "%d"
    } else {
        sequence.format.as_str()
    };
    let numbers: Vec<i64> = if start <= end {
        (start..=end).collect()
    } else {
        (end..=start).rev().collect()
    };
    Ok(numbers
        .into_iter()
        .map(|number| Value::String(printf(format, number)))
        .collect())
}

/// Go's `fmt.Sprintf(format, number)` for the verbs a sequence format uses.
fn printf(format: &str, number: i64) -> String {
    let mut out = String::new();
    let mut characters = format.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '%' {
            out.push(character);
            continue;
        }
        let mut flags = String::new();
        while let Some(&next) = characters.peek() {
            if next.is_ascii_digit() || next == '-' || next == '+' || next == ' ' {
                flags.push(next);
                characters.next();
            } else {
                break;
            }
        }
        let Some(verb) = characters.next() else {
            out.push('%');
            break;
        };
        let body = match verb {
            'd' | 'v' => number.to_string(),
            'x' => format!("{number:x}"),
            'X' => format!("{number:X}"),
            'o' => format!("{number:o}"),
            'b' => format!("{number:b}"),
            '%' => {
                out.push('%');
                continue;
            }
            other => format!("%!{other}(int={number})"),
        };
        let zero = flags.starts_with('0');
        let left = flags.contains('-');
        let width: usize = flags
            .trim_start_matches(['0', '-', '+', ' '])
            .parse()
            .unwrap_or(0);
        let body = if flags.contains('+') && number >= 0 {
            format!("+{body}")
        } else {
            body
        };
        if body.len() >= width {
            out.push_str(&body);
        } else if left {
            out.push_str(&format!("{body:<width$}"));
        } else if zero {
            let (sign, digits) = match body.strip_prefix(['-', '+']) {
                Some(digits) => (&body[..1], digits),
                None => ("", body.as_str()),
            };
            out.push_str(sign);
            out.push_str(&"0".repeat(width - body.len()));
            out.push_str(digits);
        } else {
            out.push_str(&format!("{body:>width$}"));
        }
    }
    out
}

/// A YAML value as the JSON value Argo holds in an `Item`.
pub(crate) fn yaml_value(node: &Node) -> Value {
    match &node.value {
        Yaml::Null => Value::Nil,
        Yaml::Bool(flag) => Value::Bool(*flag),
        Yaml::Integer(number) => Value::Float(*number as f64),
        Yaml::Unsigned(number) => Value::Float(*number as f64),
        Yaml::Float(number) => Value::Float(*number),
        Yaml::String(text) => Value::String(text.clone()),
        Yaml::Sequence(items) => Value::Array(items.iter().map(yaml_value).collect()),
        Yaml::Mapping(entries) => Value::Map(
            entries
                .iter()
                .map(|entry| (entry.key.clone(), yaml_value(&entry.value)))
                .collect(),
        ),
    }
}

/// An item as text, as `fmt.Sprintf("%v", item)` prints Argo's `Item`: numbers as JSON.
fn item_text(item: &Value) -> String {
    match item {
        Value::Float(number) => argo_parser::model::json_float(*number),
        Value::String(text) => text.clone(),
        Value::Array(_) | Value::Map(_) => item.to_json(),
        other => other.to_go_string(),
    }
}

/// The variables of one item, and the name of its node: `name(index:description)`.
pub(crate) fn item_variables(
    name: &str,
    index: usize,
    item: &Value,
) -> Result<(BTreeMap<String, String>, String), String> {
    let mut values = BTreeMap::new();
    let description = match item {
        Value::Map(map) => {
            let mut parts = Vec::new();
            for (key, value) in map {
                values.insert(format!("item.{key}"), value.to_go_string());
                parts.push(format!("{key}:{}", value.to_go_string()));
            }
            values.insert("item".to_owned(), item.to_json());
            parts.sort();
            parts.join(",")
        }
        Value::Array(_) => {
            values.insert("item".to_owned(), item.to_json());
            item.to_go_string()
        }
        Value::Nil => {
            return Err(format!(
                "withItems[{index}] expected string, number, list, or map. received: null"
            ));
        }
        other => {
            let text = item_text(other);
            values.insert("item".to_owned(), text.clone());
            text
        }
    };
    let description = description.replace(['(', ')'], "");
    Ok((values, format!("{name}({index}:{description})")))
}

/// The items of a loop whose list is not valid: none when the step or task will not run, as
/// Argo ignores the list then, or else the error.
pub(crate) fn items_unless_skipped(
    when: &str,
    list: Result<Vec<Value>, String>,
) -> Result<Vec<Value>, String> {
    match list {
        Err(_) if matches!(crate::when::should_execute(when), Ok(false)) => Ok(Vec::new()),
        list => list,
    }
}

/// The step or task of one item: its tags substituted, without the loop fields. With
/// `strict`, a simple tag that stays unresolved is an error, as Argo's `processItem` has it;
/// expression tags that cannot run stay, as a run with empty outputs leaves some.
pub(crate) fn substitute_item(
    node: &Node,
    variables: &Variables,
    strict: bool,
) -> Result<Node, String> {
    let mut node = node.clone();
    if let Yaml::Mapping(entries) = &mut node.value {
        entries.retain(|entry| !LOOP_FIELDS.contains(&entry.key.as_str()));
        // Artifact arguments were resolved before the loop, as Argo resolves them.
        for entry in entries.iter_mut() {
            if entry.key == "arguments"
                && let Yaml::Mapping(arguments) = &mut entry.value.value
            {
                arguments.retain(|argument| argument.key != "artifacts");
            }
        }
    }
    substitute(&mut node, variables, strict)?;
    Ok(node)
}

/// Whether an item is not known, as the empty text of a mock run's output is.
pub(crate) fn unknown_item(item: &Value) -> bool {
    matches!(item, Value::String(text) if text.is_empty())
}

fn substitute(node: &mut Node, variables: &Variables, strict: bool) -> Result<(), String> {
    match &mut node.value {
        Yaml::String(text) if text.contains("{{") => {
            let replaced = replace(text, variables, true)?;
            if strict && let Some(tag) = unresolved_simple_tag(&replaced) {
                return Err(format!("failed to resolve {{{{{tag}}}}}"));
            }
            *text = replaced;
        }
        Yaml::Sequence(items) => {
            for item in items {
                substitute(item, variables, strict)?;
            }
        }
        Yaml::Mapping(entries) => {
            for entry in entries {
                substitute(&mut entry.value, variables, strict)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// The first simple tag of a text, like `steps.a.outputs.result` in `x {{steps.a.outputs.result}}`.
fn unresolved_simple_tag(text: &str) -> Option<String> {
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let end = after.find("}}")?;
        let tag = &after[..end];
        if !tag.starts_with('=') {
            return Some(tag.to_owned());
        }
        rest = &after[end + 2..];
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequences_follow_go() {
        let sequence = |start: &str, end: &str, count: &str, format: &str| Sequence {
            start: (!start.is_empty()).then(|| argo_parser::model::AnyString(start.into())),
            end: (!end.is_empty()).then(|| argo_parser::model::AnyString(end.into())),
            count: (!count.is_empty()).then(|| argo_parser::model::AnyString(count.into())),
            format: format.into(),
        };
        let texts =
            |items: Vec<Value>| -> Vec<String> { items.iter().map(Value::to_go_string).collect() };
        assert_eq!(
            texts(expand_sequence(&sequence("", "", "3", "")).expect("a list")),
            ["0", "1", "2"]
        );
        assert_eq!(
            texts(expand_sequence(&sequence("5", "3", "", "")).expect("a list")),
            ["5", "4", "3"]
        );
        assert_eq!(
            texts(expand_sequence(&sequence("1", "2", "", "file-%02d.txt")).expect("a list")),
            ["file-01.txt", "file-02.txt"]
        );
    }

    #[test]
    fn items_name_nodes_like_argo() {
        let map = Value::parse_json(r#"{"os": "debian", "version": 9}"#).expect("JSON");
        let (variables, name) = item_variables("print", 1, &map).expect("an item");
        assert_eq!(name, "print(1:os:debian,version:9)");
        assert_eq!(variables.get("item.version").map(String::as_str), Some("9"));
        let (variables, name) = item_variables("print", 0, &Value::Float(2.0)).expect("an item");
        assert_eq!(name, "print(0:2)");
        assert_eq!(variables.get("item").map(String::as_str), Some("2"));
        let list = Value::parse_json(r#"["a", "b(c)"]"#).expect("JSON");
        let (_, name) = item_variables("x", 2, &list).expect("an item");
        assert_eq!(name, "x(2:[a bc])");
    }
}
