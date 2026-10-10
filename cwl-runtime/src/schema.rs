//! Describes CWL process inputs and outputs as JSON Schema.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;

use cwl_parser::model::{
    Class, Expression, InputParameter, OutputParameter, Process, Type, short_name,
};
use cwl_parser::salad::context::Namespaces;
use input_schema::{DIRECTORY_PATH, DRAFT, FILE_PATH};
use serde_json::{Map as JsonMap, Value as Json, json};

use crate::files;
use crate::types::Types;
use crate::value::{Map, Value};

/// How deep named types may nest, which stops types that name themselves.
const DEPTH: usize = 16;

/// The schema of the inputs of a process. An integer outside CWL `long` bounds is an error.
pub fn of(process: &Process, types: &Types, namespaces: &Namespaces) -> Result<Json, String> {
    let mut properties = JsonMap::new();
    let mut required = Vec::new();
    for input in &process.inputs {
        let name = short_name(&input.id).to_owned();
        let mut schema = parameter(input, types, namespaces);
        if let Some(default) = &input.default {
            schema["default"] = to_json(&Value::from_node(default))?;
        } else if !types.optional(&input.value_type) {
            required.push(Json::String(name.clone()));
        }
        properties.insert(name, schema);
    }
    Ok(object(process, properties, required))
}

/// The schema of the outputs of a process: the output object of a run. A file or directory is
/// a path that the run makes.
pub fn outputs(process: &Process, types: &Types, namespaces: &Namespaces) -> Json {
    let mut properties = JsonMap::new();
    let mut required = Vec::new();
    for output in &process.outputs {
        let name = short_name(&output.id).to_owned();
        if !types.optional(&output.value_type) {
            required.push(Json::String(name.clone()));
        }
        properties.insert(name, output_schema(output, types, namespaces));
    }
    object(process, properties, required)
}

/// The schema of an object of a process: its label as the title, else its name, and its
/// documentation as the description.
fn object(process: &Process, properties: JsonMap<String, Json>, required: Vec<Json>) -> Json {
    let mut schema = json!({
        "$schema": DRAFT,
        "title": process
            .label
            .clone()
            .unwrap_or_else(|| crate::process_name(&process.id)),
        "type": "object",
        "properties": properties,
    });
    if !process.doc.is_empty() {
        schema["description"] = json!(process.doc.join("\n").trim());
    }
    if !required.is_empty() {
        schema["required"] = Json::Array(required);
    }
    schema
}

/// The schema of one output: its type, whose files and directories need not exist before the
/// run, with the format and secondary files of a file.
fn output_schema(output: &OutputParameter, types: &Types, namespaces: &Namespaces) -> Json {
    let mut schema = type_schema(&output.value_type, types, namespaces, 0);
    without_exists(&mut schema);
    if let Some(label) = &output.label {
        schema["title"] = json!(label);
    }
    if !output.doc.is_empty() {
        schema["description"] = json!(output.doc.join("\n").trim());
    }
    let formats: Vec<String> = (output.format.iter())
        .filter_map(literal)
        .map(|format| namespaces.expand_prefix(format))
        .collect();
    let secondary: Vec<&str> = (output.secondary_files.iter())
        .filter_map(|file| literal(&file.pattern))
        .collect();
    if !formats.is_empty() || !secondary.is_empty() {
        with_file_details(&mut schema, &formats, &secondary);
    }
    schema
}

/// A schema without the `exists` keywords of its files and directories.
fn without_exists(schema: &mut Json) {
    match schema {
        Json::Object(fields) => {
            fields.remove("exists");
            fields.values_mut().for_each(without_exists);
        }
        Json::Array(items) => items.iter_mut().for_each(without_exists),
        _ => {}
    }
}

/// The schema of one input: its type, with the formats and secondary files of a file.
fn parameter(input: &InputParameter, types: &Types, namespaces: &Namespaces) -> Json {
    let mut schema = type_schema(&input.value_type, types, namespaces, 0);
    if let Json::Object(fields) = &mut schema {
        if let Some(label) = &input.label {
            fields.insert("title".into(), json!(label));
        }
        if !input.doc.is_empty() {
            fields.insert("description".into(), json!(input.doc.join("\n").trim()));
        }
    }
    let formats: Vec<String> = input
        .format
        .iter()
        .filter_map(literal)
        .map(|format| namespaces.expand_prefix(format))
        .collect();
    let secondary: Vec<&str> = input
        .secondary_files
        .iter()
        .filter_map(|file| literal(&file.pattern))
        .collect();
    if !formats.is_empty() || !secondary.is_empty() {
        with_file_details(&mut schema, &formats, &secondary);
    }
    schema
}

/// Puts the formats and secondary files on the file schemas inside a schema: the schema of a
/// file, or of the items of a list of files.
fn with_file_details(schema: &mut Json, formats: &[String], secondary: &[&str]) {
    let Json::Object(fields) = schema else {
        return;
    };
    if fields.get("format").and_then(Json::as_str) == Some(FILE_PATH) {
        match formats {
            [] => {}
            [one] => {
                fields.insert("fileFormat".into(), json!(one));
            }
            many => {
                fields.insert("fileFormat".into(), json!(many));
            }
        }
        if !secondary.is_empty() {
            fields.insert("secondaryFiles".into(), json!(secondary));
        }
        return;
    }
    for keyword in ["items", "anyOf"] {
        match fields.get_mut(keyword) {
            Some(Json::Array(branches)) => branches
                .iter_mut()
                .for_each(|branch| with_file_details(branch, formats, secondary)),
            Some(branch) => with_file_details(branch, formats, secondary),
            None => {}
        }
    }
}

/// The text of an expression without code, like a format IRI or a secondary file suffix.
fn literal(expression: &Expression) -> Option<&str> {
    let text = expression.text.as_str();
    (!text.contains("$(") && !text.contains("${")).then_some(text)
}

/// The JSON Schema of a CWL type. `null` in a union makes the value optional, which the
/// `required` list of the object says, so the union keeps its other members.
fn type_schema(value_type: &Type, types: &Types, namespaces: &Namespaces, depth: usize) -> Json {
    if depth > DEPTH {
        return json!({});
    }
    match types.resolve(value_type) {
        Type::Null => json!({"type": "null"}),
        Type::Boolean => json!({"type": "boolean"}),
        Type::Int | Type::Long => json!({"type": "integer"}),
        Type::Float | Type::Double => json!({"type": "number"}),
        Type::String => json!({"type": "string"}),
        Type::File | Type::Stdin | Type::Stdout | Type::Stderr => {
            json!({"type": "string", "format": FILE_PATH, "exists": true})
        }
        Type::Directory => json!({"type": "string", "format": DIRECTORY_PATH, "exists": true}),
        Type::Any | Type::Named(_) => json!({}),
        Type::Array(array) => {
            json!({"type": "array", "items": type_schema(&array.items, types, namespaces, depth + 1)})
        }
        Type::Enum(schema) => {
            let symbols: Vec<&str> = schema.symbols.iter().map(|s| short_name(s)).collect();
            json!({"type": "string", "enum": symbols})
        }
        Type::Record(record) => {
            let mut properties = JsonMap::new();
            let mut required = Vec::new();
            for field in &record.fields {
                let mut schema = type_schema(&field.value_type, types, namespaces, depth + 1);
                if let Some(label) = &field.label {
                    schema["title"] = json!(label);
                }
                if !field.doc.is_empty() {
                    schema["description"] = json!(field.doc.join("\n").trim());
                }
                let formats: Vec<String> = field
                    .format
                    .iter()
                    .filter_map(literal)
                    .map(|format| namespaces.expand_prefix(format))
                    .collect();
                let secondary: Vec<&str> = field
                    .secondary_files
                    .iter()
                    .filter_map(|file| literal(&file.pattern))
                    .collect();
                if !formats.is_empty() || !secondary.is_empty() {
                    with_file_details(&mut schema, &formats, &secondary);
                }
                if !types.optional(&field.value_type) {
                    required.push(json!(field.name));
                }
                properties.insert(field.name.clone(), schema);
            }
            let mut schema = json!({"type": "object", "properties": properties});
            if !required.is_empty() {
                schema["required"] = Json::Array(required);
            }
            schema
        }
        Type::Union(members) => {
            let members: Vec<Json> = members
                .iter()
                .filter(|member| !matches!(types.resolve(member), Type::Null))
                .map(|member| type_schema(member, types, namespaces, depth + 1))
                .collect();
            let simple: Option<Vec<&Json>> = members
                .iter()
                .map(|member| match member.as_object() {
                    Some(fields) if fields.len() == 1 => fields.get("type"),
                    _ => None,
                })
                .collect();
            match (members.as_slice(), simple) {
                ([], _) => json!({"type": "null"}),
                ([one], _) => one.clone(),
                (_, Some(kinds)) => json!({"type": kinds}),
                (many, None) => json!({"anyOf": many}),
            }
        }
    }
}

/// The input object of a run as JSON, with files and directories as their paths or addresses.
/// A `null` is no value in CWL, so it is left out. Integers must fit a CWL `long`.
pub fn values(job: &Map) -> Result<Json, String> {
    to_json(&Value::Map(job.clone()))
}

/// A value as JSON: a file or directory is its local path, else its address; a literal file
/// without either is a `data:` address of its content.
fn to_json(value: &Value) -> Result<Json, String> {
    Ok(match value {
        Value::Null => Json::Null,
        Value::Bool(value) => json!(value),
        Value::Integer(value) => json!(value),
        Value::BigInteger(digits) => {
            let value = digits
                .parse::<i64>()
                .map_err(|_| format!("integer `{digits}` is outside the range of a CWL long"))?;
            json!(value)
        }
        Value::Float(value) => {
            serde_json::Number::from_f64(*value).map_or(Json::Null, Json::Number)
        }
        Value::String(text) => json!(text),
        Value::List(items) => {
            Json::Array(items.iter().map(to_json).collect::<Result<Vec<_>, _>>()?)
        }
        Value::Map(map) if value.is_file_like() => {
            let location = map
                .get("path")
                .or_else(|| map.get("location"))
                .and_then(Value::as_str);
            match location {
                Some(location) => match files::local_path(location) {
                    Some(path) => json!(path.to_string_lossy()),
                    None => json!(location),
                },
                None => {
                    let content = map.get("contents").and_then(Value::as_str);
                    json!(data_address(content.unwrap_or_default()))
                }
            }
        }
        Value::Map(map) => Json::Object(
            map.iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(key, value)| Ok((key.clone(), to_json(value)?)))
                .collect::<Result<JsonMap<_, _>, String>>()?,
        ),
    })
}

/// A `data:` address that holds a text, percent-encoded.
fn data_address(text: &str) -> String {
    let mut address = String::from("data:,");
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            address.push(char::from(byte));
        } else {
            address.push_str(&format!("%{byte:02X}"));
        }
    }
    address
}

/// Finds inputs read through workflow sources, command-line bindings and tool expressions.
pub fn read(process: &Process) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    match &process.class {
        Class::Workflow(workflow) => {
            let sources = workflow
                .steps
                .iter()
                .flat_map(|step| &step.inputs)
                .flat_map(|input| &input.source)
                .chain(
                    process
                        .outputs
                        .iter()
                        .flat_map(|output| &output.output_source),
                );
            for source in sources {
                let own = process
                    .inputs
                    .iter()
                    .find(|input| &input.id == source || short_name(&input.id) == source);
                if let Some(input) = own {
                    names.insert(short_name(&input.id).to_owned());
                }
            }
        }
        class => {
            for input in &process.inputs {
                if input.binding.is_some() && matches!(class, Class::CommandLineTool(_)) {
                    names.insert(short_name(&input.id).to_owned());
                }
            }
            let bindings: Vec<_> = process.inputs.iter().map(|input| &input.binding).collect();
            let text = format!(
                "{class:?}{:?}{:?}{:?}{bindings:?}",
                process.requirements, process.hints, process.outputs
            );
            names.extend(referenced(&text));
        }
    }
    names
}

/// The input names that a text refers to as `inputs.name`, `inputs['name']` or
/// `inputs["name"]`, also with the quotes escaped as debug text writes them. A name followed by
/// `(`, like `inputs.hasOwnProperty(`, is a method, not an input.
fn referenced(text: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    // Only where an expression can start: not inside a name, a path or an address, like
    // `array-inputs.cwl`.
    let inside = |c: char| word(c) || matches!(c, '-' | '.' | '/' | '#');
    for (start, _) in text.match_indices("inputs") {
        if text[..start].chars().next_back().is_some_and(inside) {
            continue;
        }
        let rest = &text[start + "inputs".len()..];
        let name: String = if let Some(rest) = rest.strip_prefix('.') {
            let name: String = rest.chars().take_while(|c| word(*c)).collect();
            if rest[name.len()..].starts_with('(') {
                continue;
            }
            name
        } else if let Some(rest) = rest.strip_prefix('[') {
            let rest = rest.trim_start_matches('\\');
            let Some(quote) = rest.chars().next().filter(|c| matches!(c, '\'' | '"')) else {
                continue;
            };
            let rest = &rest[1..];
            rest.chars()
                .take_while(|c| *c != quote && *c != '\\')
                .collect()
        } else {
            continue;
        };
        if !name.is_empty() {
            names.insert(name);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_in_expressions() {
        let text = r#"$(inputs.reads.path) ${ return inputs["threads"] + inputs['mode'] } inputs[\"index\"] myinputs.x inputs.hasOwnProperty("y") file:///a/array-inputs.cwl#x"#;
        let found: Vec<String> = referenced(text).into_iter().collect();
        assert_eq!(found, ["index", "mode", "reads", "threads"]);
    }

    #[test]
    fn encodes_literal_addresses() {
        assert_eq!(data_address("a b\n"), "data:,a%20b%0A");
    }
}
