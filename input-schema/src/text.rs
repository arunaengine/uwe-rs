//! The inputs of a workflow as text, for help on the command line: each input with its kind,
//! whether it is required, its default, its choices and its description, in the groups of the
//! schema.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::Value as Json;

use crate::properties::{Property, properties, resolve};
use crate::{DIRECTORY_PATH, FILE_PATH, FILE_PATH_PATTERN, PATH};

/// The help text of a schema, with the groups in the order of `allOf` and the inputs of a
/// group by name. Inputs that nf-schema marks `hidden` are left out and counted at the end.
pub fn text(schema: &Json) -> String {
    let mut out = String::new();
    for keyword in ["title", "description"] {
        if let Some(text) = schema.get(keyword).and_then(Json::as_str) {
            out.push_str(text.trim());
            out.push('\n');
        }
    }
    let mut group: Option<Option<&str>> = None;
    let mut hidden = 0;
    for property in properties(schema) {
        if property.schema.get("hidden") == Some(&Json::Bool(true)) {
            hidden += 1;
            continue;
        }
        if group != Some(property.group) {
            group = Some(property.group);
            out.push('\n');
            out.push_str(property.group.unwrap_or("Inputs"));
            out.push('\n');
        }
        out.push_str(&line(schema, &property));
    }
    match hidden {
        0 => {}
        1 => out.push_str("\n1 hidden input is not shown.\n"),
        count => out.push_str(&format!("\n{count} hidden inputs are not shown.\n")),
    }
    out
}

/// `  --name <kind> (required)` and the description below it.
fn line(root: &Json, property: &Property) -> String {
    let (schema, _) = resolve(root, property.schema);
    let mut notes = Vec::new();
    if property.required {
        notes.push("required".to_owned());
    }
    if let Some(Json::Array(choices)) = schema.get("enum") {
        let choices: Vec<String> = choices.iter().map(plain).collect();
        notes.push(format!("one of: {}", choices.join(", ")));
    }
    if let Some(default) = schema.get("default") {
        notes.push(format!("default: {}", plain(default)));
    }
    let mut line = format!("  --{} <{}>", property.name, kind(root, schema));
    if !notes.is_empty() {
        line.push_str(&format!(" ({})", notes.join("; ")));
    }
    line.push('\n');
    let description = schema
        .get("description")
        .or_else(|| schema.get("title"))
        .and_then(Json::as_str);
    if let Some(description) = description {
        for text in description.lines().filter(|text| !text.trim().is_empty()) {
            line.push_str(&format!("      {}\n", text.trim()));
        }
    }
    line
}

/// What kind of value an input takes, in words.
fn kind(root: &Json, schema: &Json) -> String {
    match schema.get("format").and_then(Json::as_str) {
        Some(FILE_PATH) => return "file".into(),
        Some(DIRECTORY_PATH) => return "directory".into(),
        Some(PATH) => return "file or directory".into(),
        Some(FILE_PATH_PATTERN) => return "file pattern".into(),
        _ => {}
    }
    let kinds: Vec<&str> = match schema.get("type") {
        Some(Json::String(kind)) => vec![kind],
        Some(Json::Array(kinds)) => kinds.iter().filter_map(Json::as_str).collect(),
        _ => Vec::new(),
    };
    let words: Vec<String> = kinds
        .iter()
        .filter(|name| **name != "null")
        .map(|name| match *name {
            "string" => "text".to_owned(),
            "array" => match schema.get("items") {
                Some(items) => format!("list of {}", kind(root, resolve(root, items).0)),
                None => "list".to_owned(),
            },
            other => other.to_owned(),
        })
        .collect();
    match words.is_empty() {
        true => "value".into(),
        false => words.join(" or "),
    }
}

fn plain(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn help_in_groups() {
        let schema = json!({
            "title": "demo",
            "$defs": {"io": {
                "title": "Input/output options",
                "required": ["input"],
                "properties": {
                    "input": {"type": "string", "format": "file-path", "description": "The sample sheet."},
                    "aligner": {"type": "string", "enum": ["star", "hisat2"], "default": "star"},
                    "help": {"type": "boolean", "hidden": true}
                }
            }},
            "allOf": [{"$ref": "#/$defs/io"}],
            "properties": {"lanes": {"type": "array", "items": {"type": "integer"}}}
        });
        assert_eq!(
            text(&schema),
            "demo\n\nInput/output options\n  --aligner <text> (one of: star, hisat2; default: \
             star)\n  --input <file> (required)\n      The sample sheet.\n\nInputs\n  --lanes <list \
             of integer>\n\n1 hidden input is not shown.\n"
        );
    }
}
