//! YAML text of tool definitions and composed workflows: reads it as JSON values and writes
//! JSON values as block-style YAML that people can read and edit.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt::Write as _;

use saphyr::{LoadableYamlNode, Scalar, Yaml};
use serde_json::{Map, Number, Value as Json};

/// The first document of a YAML text as a JSON value; an empty text is null. Tags are dropped
/// and keys become text, as JSON has only text keys.
pub fn read(text: &str) -> Result<Json, String> {
    let documents = Yaml::load_from_str(text).map_err(|error| error.to_string())?;
    match documents.first() {
        Some(document) => value(document),
        None => Ok(Json::Null),
    }
}

/// The YAML text of a JSON value, in block style with two spaces per level, ending with a
/// line break. [`read`] gives the same value back.
pub fn write(value: &Json) -> String {
    let mut text = String::new();
    match value {
        Json::Object(entries) if !entries.is_empty() => object(&mut text, entries, 0),
        Json::Array(items) if !items.is_empty() => array(&mut text, items, 0),
        // A literal block at the root needs its lines indented below its marker too.
        other => {
            text.push_str(&scalar(other, 2));
            text.push('\n');
        }
    }
    text
}

fn value(node: &Yaml) -> Result<Json, String> {
    Ok(match node {
        Yaml::Value(scalar) => scalar_value(scalar),
        Yaml::Representation(text, _, _) => Json::String(text.to_string()),
        Yaml::Sequence(items) => Json::Array(items.iter().map(value).collect::<Result<_, _>>()?),
        Yaml::Mapping(entries) => {
            let mut object = Map::new();
            for (key, item) in entries {
                object.insert(key_text(key)?, value(item)?);
            }
            Json::Object(object)
        }
        Yaml::Tagged(_, inner) => value(inner)?,
        Yaml::Alias(_) => return Err("YAML aliases are not supported".to_owned()),
        Yaml::BadValue => return Err("a value is not valid YAML".to_owned()),
    })
}

fn scalar_value(scalar: &Scalar) -> Json {
    match scalar {
        Scalar::Null => Json::Null,
        Scalar::Boolean(flag) => Json::Bool(*flag),
        Scalar::Integer(number) => Json::from(*number),
        Scalar::FloatingPoint(number) => {
            Number::from_f64(number.0).map_or(Json::Null, Json::Number)
        }
        Scalar::String(text) => Json::String(text.to_string()),
    }
}

fn key_text(key: &Yaml) -> Result<String, String> {
    match value(key)? {
        Json::String(text) => Ok(text),
        Json::Null => Ok("null".to_owned()),
        Json::Array(_) | Json::Object(_) => Err("a YAML key is not a scalar".to_owned()),
        other => Ok(other.to_string()),
    }
}

fn object(text: &mut String, entries: &Map<String, Json>, indent: usize) {
    for (key, item) in entries {
        let _ = write!(text, "{}{}:", " ".repeat(indent), key_scalar(key));
        nested(text, item, indent);
    }
}

fn array(text: &mut String, items: &[Json], indent: usize) {
    for item in items {
        let _ = write!(text, "{}-", " ".repeat(indent));
        match item {
            // The first entry of an object shares the line of its dash.
            Json::Object(entries) if !entries.is_empty() => {
                let mut inner = String::new();
                object(&mut inner, entries, indent + 2);
                text.push(' ');
                text.push_str(inner.trim_start());
            }
            other => nested(text, other, indent),
        }
    }
}

/// The value after a key or dash: on the same line for scalars, else on the lines below.
fn nested(text: &mut String, item: &Json, indent: usize) {
    match item {
        Json::Object(entries) if !entries.is_empty() => {
            text.push('\n');
            object(text, entries, indent + 2);
        }
        Json::Array(items) if !items.is_empty() => {
            text.push('\n');
            array(text, items, indent + 2);
        }
        other => {
            text.push(' ');
            text.push_str(&scalar(other, indent + 2));
            text.push('\n');
        }
    }
}

/// A scalar, an empty array or an empty object on one line, or a text of several lines as a
/// literal block indented by `indent`.
fn scalar(item: &Json, indent: usize) -> String {
    match item {
        Json::Null => "null".to_owned(),
        Json::Bool(flag) => flag.to_string(),
        Json::Number(number) => number.to_string(),
        Json::Array(_) => "[]".to_owned(),
        Json::Object(_) => "{}".to_owned(),
        Json::String(text) if block(text) => {
            let lines = text.strip_suffix('\n').unwrap_or(text);
            let marker = if text.ends_with('\n') { "|" } else { "|-" };
            let pad = " ".repeat(indent);
            let body: Vec<String> = (lines.split('\n'))
                .map(|line| match line.is_empty() {
                    true => String::new(),
                    false => format!("{pad}{line}"),
                })
                .collect();
            format!("{marker}\n{}", body.join("\n"))
        }
        Json::String(text) => key_scalar(text),
    }
}

/// Whether a text of several lines can be a literal block: its lines keep their spaces.
fn block(text: &str) -> bool {
    let lines = text.strip_suffix('\n').unwrap_or(text);
    lines.contains('\n')
        && !lines.starts_with([' ', '\t'])
        && !lines.ends_with('\n')
        && !text.chars().any(|c| c.is_control() && c != '\n')
        && !lines.split('\n').any(|line| line.ends_with([' ', '\t']))
}

/// A text as a plain scalar where YAML reads it back as the same text, else double-quoted.
fn key_scalar(text: &str) -> String {
    const INDICATORS: &str = "-?:,[]{}#&*!|>'\"%@`";
    let reserved = matches!(
        text.to_ascii_lowercase().as_str(),
        "null" | "~" | "true" | "false" | "yes" | "no" | "on" | "off" | ".nan" | ".inf" | "-.inf"
    );
    let numeric = text.parse::<f64>().is_ok() || text.starts_with("0x") || text.starts_with("0o");
    let plain = !text.is_empty()
        && !reserved
        && !numeric
        && !text.starts_with(|c: char| INDICATORS.contains(c) || c.is_whitespace())
        && !text.ends_with(char::is_whitespace)
        && !text.contains(": ")
        && !text.contains(" #")
        && !text.ends_with(':')
        && !text.chars().any(char::is_control);
    match plain {
        true => text.to_owned(),
        false => Json::String(text.to_owned()).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn round_trip() {
        let value = json!({
            "uwe_version": "1",
            "name": "align: reads",
            "count": 3,
            "ratio": 0.5,
            "empty": {},
            "none": [],
            "flags": [true, null, "yes", "12", "-x"],
            "steps": [{"id": "a", "tool": "fastqc", "with": {"reads": "in.fq"}}, {"id": "b"}],
            "doc": "first line\nsecond line\n",
            "spaced": " lead",
        });
        let text = write(&value);
        assert_eq!(read(&text), Ok(value), "{text}");
        assert!(text.contains("- id: a\n    tool: fastqc\n"), "{text}");
        assert!(
            text.contains("doc: |\n  first line\n  second line\n"),
            "{text}"
        );
    }

    #[test]
    fn round_trips_roots() {
        for value in [
            json!("first\nsecond\n"),
            json!("first\n\nthird"),
            json!("one"),
        ] {
            let text = write(&value);
            assert_eq!(read(&text), Ok(value), "{text}");
        }
    }

    #[test]
    fn reads_documents() {
        assert_eq!(read(""), Ok(Json::Null));
        let text = "# a comment\nname: x\nitems:\n  - 1\n  - two\n";
        assert_eq!(read(text), Ok(json!({"name": "x", "items": [1, "two"]})));
        assert!(read("a: [").is_err());
    }
}
