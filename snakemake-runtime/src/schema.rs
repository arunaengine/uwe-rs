//! `snakemake.utils.validate`: checks a config or a sample table against a JSON schema,
//! and fills in the defaults that the schema gives, as Snakemake does with `set_default`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use python_evaluator::modules::data::{load_yaml, parse_json};
use python_evaluator::{Dict, Error, Interpreter, Key, Value};
use serde_json::Value as Json;

use crate::workflow::workflow_error;

/// A Python value as JSON, for validation.
pub fn to_json(interpreter: &mut Interpreter, value: &Value) -> Result<Json, Error> {
    Ok(match value.plain() {
        Value::None => Json::Null,
        Value::Bool(value) => Json::Bool(*value),
        Value::Int(value) => Json::from(*value),
        Value::BigInt(value) => Json::from(value.to_string().parse::<f64>().unwrap_or(0.0)),
        Value::Float(value) => {
            serde_json::Number::from_f64(*value).map_or(Json::Null, Json::Number)
        }
        Value::Str(text) => Json::String(text.to_string()),
        Value::List(_) | Value::Tuple(_) | Value::Set(_) | Value::FrozenSet(_) => {
            let items = interpreter.iterate(value)?;
            Json::Array(
                items
                    .iter()
                    .map(|item| to_json(interpreter, item))
                    .collect::<Result<_, _>>()?,
            )
        }
        Value::Dict(_) => {
            let mut object = serde_json::Map::new();
            for (key, item) in interpreter.mapping_items(value)? {
                let key = match key.plain() {
                    Value::Str(text) => text.to_string(),
                    other => python_evaluator::format::plain_str(other),
                };
                object.insert(key, to_json(interpreter, &item)?);
            }
            Json::Object(object)
        }
        other => Json::String(interpreter.to_str(other)?),
    })
}

/// JSON as a Python value.
pub fn from_json(json: &Json) -> Value {
    match json {
        Json::Null => Value::None,
        Json::Bool(value) => Value::Bool(*value),
        Json::Number(number) => match number.as_i64() {
            Some(value) => Value::Int(value),
            None => Value::Float(number.as_f64().unwrap_or(0.0)),
        },
        Json::String(text) => Value::str(text.as_str()),
        Json::Array(items) => Value::list(items.iter().map(from_json).collect()),
        Json::Object(object) => {
            let mut dict = Dict::default();
            for (key, item) in object {
                dict.entries
                    .insert(Key(Value::str(key.as_str())), from_json(item));
            }
            Value::dict(dict)
        }
    }
}

/// Reads a schema file, YAML or JSON, as JSON.
fn read_schema(path: &Path) -> Result<Json, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let value = match parse_json(&text) {
        Ok(value) => value,
        Err(_) => load_yaml(&text).map_err(|error| error.message())?,
    };
    plain_json(&value)
}

/// JSON of values read from YAML, without an interpreter.
fn plain_json(value: &Value) -> Result<Json, String> {
    Ok(match value.plain() {
        Value::None => Json::Null,
        Value::Bool(value) => Json::Bool(*value),
        Value::Int(value) => Json::from(*value),
        Value::Float(value) => {
            serde_json::Number::from_f64(*value).map_or(Json::Null, Json::Number)
        }
        Value::Str(text) => Json::String(text.to_string()),
        Value::List(items) => Json::Array(
            items
                .borrow()
                .iter()
                .map(plain_json)
                .collect::<Result<_, _>>()?,
        ),
        Value::Dict(dict) => {
            let mut object = serde_json::Map::new();
            for (key, item) in dict.borrow().entries.iter() {
                object.insert(
                    python_evaluator::format::plain_str(&key.0),
                    plain_json(item)?,
                );
            }
            Json::Object(object)
        }
        other => Json::String(python_evaluator::format::plain_str(other)),
    })
}

/// Finds files that relative `$ref`s of a schema name.
struct FileRetriever;

impl jsonschema::Retrieve for FileRetriever {
    fn retrieve(
        &self,
        uri: &jsonschema::Uri<String>,
    ) -> Result<Json, Box<dyn std::error::Error + Send + Sync>> {
        if uri.scheme().as_str() != "file" {
            return Err(format!("cannot fetch {uri}").into());
        }
        let path = percent_decode(uri.path().as_str());
        read_schema(Path::new(&path)).map_err(Into::into)
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&text[index + 1..index + 3], 16)
        {
            decoded.push(byte);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn file_uri(path: &Path) -> String {
    format!("file://{}", path.display()).replace(' ', "%20")
}

/// A schema node with the file and root it belongs to, for resolving `$ref`s.
struct Context {
    file: PathBuf,
    root: Json,
}

fn resolve_pointer<'a>(root: &'a Json, pointer: &str) -> Option<&'a Json> {
    if pointer.is_empty() || pointer == "/" {
        return Some(root);
    }
    root.pointer(pointer)
}

/// Follows a `$ref` to its schema, reading other files as needed.
fn resolve_ref(context: &Context, reference: &str) -> Option<(Json, Context)> {
    let (file_part, pointer) = reference.split_once('#').unwrap_or((reference, ""));
    if file_part.is_empty() {
        let node = resolve_pointer(&context.root, pointer)?.clone();
        return Some((
            node,
            Context {
                file: context.file.clone(),
                root: context.root.clone(),
            },
        ));
    }
    if file_part.contains("://") {
        return None;
    }
    let file = context
        .file
        .parent()
        .unwrap_or(Path::new("."))
        .join(file_part);
    let root = read_schema(&file).ok()?;
    let node = resolve_pointer(&root, pointer)?.clone();
    Some((node, Context { file, root }))
}

fn json_type_matches(interpreter: &mut Interpreter, schema: &Json, instance: &Value) -> bool {
    let Some(kind) = schema.get("type") else {
        return true;
    };
    let kinds: Vec<&str> = match kind {
        Json::String(kind) => vec![kind.as_str()],
        Json::Array(kinds) => kinds.iter().filter_map(Json::as_str).collect(),
        _ => return true,
    };
    let _ = interpreter;
    kinds.iter().any(|kind| {
        matches!(
            (*kind, instance.plain()),
            ("object", Value::Dict(_))
                | ("array", Value::List(_) | Value::Tuple(_))
                | ("string", Value::Str(_))
                | ("integer", Value::Int(_) | Value::BigInt(_))
                | ("number", Value::Int(_) | Value::BigInt(_) | Value::Float(_))
                | ("boolean", Value::Bool(_))
                | ("null", Value::None)
        )
    })
}

/// Sets the defaults of a schema into an instance, as the `properties` validator that
/// Snakemake installs does, following references and combinations of schemas.
fn set_defaults(
    interpreter: &mut Interpreter,
    schema: &Json,
    context: &Context,
    instance: &Value,
    depth: usize,
) -> Result<(), Error> {
    if depth > 64 {
        return Ok(());
    }
    if let Some(reference) = schema.get("$ref").and_then(Json::as_str)
        && let Some((target, target_context)) = resolve_ref(context, reference)
    {
        set_defaults(interpreter, &target, &target_context, instance, depth + 1)?;
    }
    for key in ["allOf"] {
        if let Some(Json::Array(schemas)) = schema.get(key) {
            for sub in schemas {
                set_defaults(interpreter, sub, context, instance, depth + 1)?;
            }
        }
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(Json::Array(schemas)) = schema.get(key)
            && let Some(sub) = schemas
                .iter()
                .find(|sub| json_type_matches(interpreter, sub, instance))
        {
            set_defaults(interpreter, sub, context, instance, depth + 1)?;
        }
    }
    match instance.plain() {
        Value::Dict(dict) => {
            if let Some(Json::Object(properties)) = schema.get("properties") {
                for (name, sub) in properties {
                    let key = Key(Value::str(name.as_str()));
                    if let Some(default) = sub.get("default") {
                        let mut dict = dict.borrow_mut();
                        dict.entries
                            .entry(key.clone())
                            .or_insert_with(|| from_json(default));
                    }
                    let child = dict.borrow().entries.get(&key).cloned();
                    if let Some(child) = child {
                        set_defaults(interpreter, sub, context, &child, depth + 1)?;
                    }
                }
            }
            if let Some(Json::Object(patterns)) = schema.get("patternProperties") {
                let entries: Vec<(Value, Value)> = dict
                    .borrow()
                    .entries
                    .iter()
                    .map(|(key, value)| (key.0.clone(), value.clone()))
                    .collect();
                for (pattern, sub) in patterns {
                    let Ok(regex) = fancy_regex::Regex::new(
                        &python_evaluator::modules::translate_regex(pattern),
                    ) else {
                        continue;
                    };
                    for (key, child) in &entries {
                        if key
                            .as_str()
                            .is_some_and(|key| regex.is_match(key).unwrap_or(false))
                        {
                            set_defaults(interpreter, sub, context, child, depth + 1)?;
                        }
                    }
                }
            }
            if let Some(sub @ Json::Object(_)) = schema.get("additionalProperties") {
                let named: Vec<String> = schema
                    .get("properties")
                    .and_then(Json::as_object)
                    .map(|properties| properties.keys().cloned().collect())
                    .unwrap_or_default();
                let entries: Vec<(Value, Value)> = dict
                    .borrow()
                    .entries
                    .iter()
                    .map(|(key, value)| (key.0.clone(), value.clone()))
                    .collect();
                for (key, child) in entries {
                    if key
                        .as_str()
                        .is_some_and(|key| !named.iter().any(|name| name == key))
                    {
                        set_defaults(interpreter, sub, context, &child, depth + 1)?;
                    }
                }
            }
        }
        Value::List(items) => {
            let items = items.borrow().clone();
            if let Some(sub @ Json::Object(_)) = schema.get("items") {
                for item in &items {
                    set_defaults(interpreter, sub, context, item, depth + 1)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// Builds a validator for a schema, as Draft 2020-12 like Snakemake, else by its `$schema`.
fn validator(schema: &Json) -> Option<jsonschema::Validator> {
    let strict = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .with_retriever(FileRetriever)
        .build(schema);
    match strict {
        Ok(validator) => Some(validator),
        Err(_) => jsonschema::options()
            .with_retriever(FileRetriever)
            .build(schema)
            .ok(),
    }
}

/// Loads a schema file relative to a folder.
pub fn load_schema(
    interpreter: &mut Interpreter,
    path: &str,
    base: &Path,
) -> Result<(Json, PathBuf), Error> {
    let file = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        base.join(path)
    };
    let file = python_evaluator::host::normalize(&file);
    let data = interpreter
        .host
        .read(&file)
        .map_err(|_| workflow_error(format!("Schema file {path} not found.")))?;
    let text = String::from_utf8_lossy(&data);
    let value = match parse_json(&text) {
        Ok(value) => value,
        Err(_) => load_yaml(&text).map_err(|error| {
            workflow_error(format!(
                "Schema file is not valid JSON or YAML: {}",
                error.message()
            ))
        })?,
    };
    let mut json = plain_json(&value).map_err(workflow_error)?;
    if let Json::Object(object) = &mut json {
        object.insert("$id".into(), Json::String(file_uri(&file)));
    }
    Ok((json, file))
}

/// Validates one record, a config or a row, and sets its defaults.
pub fn validate_record(
    interpreter: &mut Interpreter,
    record: &Value,
    schema: &Json,
    file: &Path,
    set_default: bool,
) -> Result<Result<(), String>, Error> {
    if set_default {
        let context = Context {
            file: file.to_owned(),
            root: schema.clone(),
        };
        set_defaults(interpreter, schema, &context, record, 0)?;
    }
    let Some(validator) = validator(schema) else {
        return Ok(Ok(()));
    };
    let instance = to_json(interpreter, record)?;
    let first = validator.iter_errors(&instance).next().map(|error| {
        let path = error.instance_path().to_string();
        if path.is_empty() {
            error.to_string()
        } else {
            format!("{error} (at {path})")
        }
    });
    Ok(match first {
        Some(message) => Err(message),
        None => Ok(()),
    })
}
