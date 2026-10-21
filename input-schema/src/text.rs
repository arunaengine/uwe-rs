//! Formats grouped workflow inputs and outputs with types, defaults, choices, and help text.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::Value as Json;

use crate::properties::{Property, properties, resolve};
use crate::{DIRECTORY_PATH, FILE_PATH, FILE_PATH_PATTERN, PATH};

/// The help text of a schema of inputs, with the groups in the order of `allOf` and the inputs
/// of a group by name. Inputs that nf-schema marks `hidden` are left out and counted at the end.
pub fn text(schema: &Json) -> String {
    write(schema, "Inputs", "--")
}

/// The text of a schema of outputs, like [`text`], with the outputs by their names.
pub fn outputs_text(schema: &Json) -> String {
    write(schema, "Outputs", "")
}

/// The text of a schema: `heading` for inputs outside groups, and `prefix` before each name.
fn write(schema: &Json, heading: &str, prefix: &str) -> String {
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
            out.push_str(property.group.unwrap_or(heading));
            out.push('\n');
        }
        out.push_str(&line(schema, &property, prefix));
    }
    match hidden {
        0 => {}
        1 => out.push_str("\n1 hidden input is not shown.\n"),
        count => out.push_str(&format!("\n{count} hidden inputs are not shown.\n")),
    }
    out
}

/// `  --name <kind> (required)` and the description below it.
fn line(root: &Json, property: &Property, prefix: &str) -> String {
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
    match schema.get("const") {
        Some(Json::Array(items)) if items.len() == 1 => notes.push(plain(&items[0])),
        Some(Json::Array(items)) => notes.push(format!("{} of them", items.len())),
        Some(value) => notes.push(plain(value)),
        None => {}
    }
    let mut line = format!(
        "  {prefix}{} <{}>",
        property.name,
        kind(root, schema, &mut Vec::new())
    );
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
fn kind<'a>(root: &'a Json, schema: &'a Json, active: &mut Vec<&'a Json>) -> String {
    if active.iter().any(|seen| std::ptr::eq(*seen, schema)) {
        return "value".into();
    }
    match schema.get("format").and_then(Json::as_str) {
        Some(FILE_PATH) => return "file".into(),
        Some(DIRECTORY_PATH) => return "directory".into(),
        Some(PATH) => return "file or directory".into(),
        Some(FILE_PATH_PATTERN) => return "file pattern".into(),
        _ => {}
    }
    active.push(schema);
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
                Some(items) => format!("list of {}", kind(root, resolve(root, items).0, active)),
                None => "list".to_owned(),
            },
            other => other.to_owned(),
        })
        .collect();
    active.pop();
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
    fn bounds_recursive_help() {
        let schema = json!({
            "properties": {"tree": {"$ref": "#/$defs/tree"}},
            "$defs": {"tree": {"type": "array", "items": {"$ref": "#/$defs/tree"}}}
        });
        assert_eq!(text(&schema), "\nInputs\n  --tree <list of value>\n");
    }

    #[test]
    fn outputs_by_name() {
        let schema = json!({
            "properties": {
                "report": {"type": "string", "format": "file-path", "const": "results/report.html"},
                "counts": {"type": "array", "items": {"type": "string", "format": "file-path"}, "const": ["a", "b"]}
            }
        });
        assert_eq!(
            outputs_text(&schema),
            "\nOutputs\n  counts <list of file> (2 of them)\n  report <file> (results/report.html)\n"
        );
    }

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
