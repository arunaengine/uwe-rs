//! nf-schema: parameter validation and summaries, and samplesheets as lists, from JSON
//! schemas (JSON Schema 2020-12, through the `jsonschema` crate).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod params;
pub(crate) mod paths;
mod samplesheet;

use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, parse_json};
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::{Key, PathValue, State, Value};

use crate::Context;

pub(crate) fn call(
    name: &str,
    arguments: Vec<Value>,
    context: &mut Context,
) -> Result<Value, Error> {
    // Named arguments come first as a map; nf-schema also takes a map of options as the last
    // argument of `samplesheetToList`.
    match (name, arguments.as_slice()) {
        ("samplesheetToList", [sheet, schema]) => {
            samplesheet::to_list(sheet, schema, None, context)
        }
        ("samplesheetToList", [options @ Value::Map(_), sheet, schema])
        | ("samplesheetToList", [sheet, schema, options]) => {
            samplesheet::to_list(sheet, schema, Some(options), context)
        }
        ("validate", [options @ Value::Map(_), input, schema]) => {
            validate(Some(options), input, schema, context)
        }
        ("validate", [input, schema]) => validate(None, input, schema, context),
        ("validateParameters", []) => params::validate(None, context),
        ("validateParameters", [options]) => params::validate(Some(options), context),
        ("paramsSummaryMap", [workflow]) => params::summary(None, workflow, context),
        ("paramsSummaryMap", [options, workflow]) => {
            params::summary(Some(options), workflow, context)
        }
        ("paramsSummaryLog", [workflow]) => params::summary_log(None, workflow, context),
        ("paramsSummaryLog", [options, workflow]) => {
            params::summary_log(Some(options), workflow, context)
        }
        ("paramsHelp", []) => params::help(None, context),
        ("paramsHelp", [options @ Value::Map(_), ..]) => params::help(Some(options), context),
        _ => Err(Error::Unsupported {
            message: format!(
                "the function `{name}` of the plugin `nf-schema` with these arguments"
            ),
            span: Default::default(),
        }),
    }
}

/// `validate`: the errors of a value against a schema. With `exitOnError`, which is on by
/// default, errors fail the call instead.
fn validate(
    options: Option<&Value>,
    input: &Value,
    schema_path: &Value,
    context: &mut Context,
) -> Result<Value, Error> {
    let exit = options
        .and_then(|options| get(options, "exitOnError"))
        .is_none_or(|exit| exit.truth());
    let schema = schema(schema_path, context)?;
    let instance = json(input);
    let validator = paths::validator(&schema.json, &instance, context, false)
        .map_err(|error| thrown(format!("Invalid JSON schema: {error}")))?;
    let errors: Vec<String> = errors(&validator, &instance, "object", context, None)
        .into_iter()
        .map(|(location, message)| {
            let value = instance.pointer(&location).map(plain).unwrap_or_default();
            match location.as_str() {
                "" => format!("({value}): {message}"),
                location => format!("{location} ({value}): {message}"),
            }
        })
        .collect();
    if exit && !errors.is_empty() {
        return Err(thrown(format!("{}\n", errors.join("\n"))));
    }
    Ok(Value::list(errors.into_iter().map(Value::string).collect()))
}

/// The value of a key of a map, or a property of the `workflow` object.
pub(crate) fn get(map: &Value, key: &str) -> Option<Value> {
    match map {
        Value::Map(map) => map.borrow().entries.get(&Key(Value::string(key))).cloned(),
        Value::Object(object) => match &object.borrow().state {
            State::Workflow(workflow) => get(&workflow.properties, key),
            _ => None,
        },
        _ => None,
    }
}

/// The entries of a map, with the keys as text.
pub(crate) fn entries(map: &Value) -> Vec<(String, Value)> {
    match map {
        Value::Map(map) => map
            .borrow()
            .entries
            .iter()
            .map(|(key, value)| (key.0.to_string(), value.clone()))
            .collect(),
        _ => Vec::new(),
    }
}

/// A path from a path or its text; relative text starts at `base`.
pub(crate) fn path(value: &Value, base: &str) -> PathValue {
    match value {
        Value::Path(path) => (**path).clone(),
        other => PathValue::parse(&other.to_string(), base),
    }
}

/// A JSON schema: its text and its tree with keys in file order.
pub(crate) struct Schema {
    pub json: serde_json::Value,
    pub tree: Value,
}

/// Reads a schema. A relative path starts at the project directory, as nf-schema does.
pub(crate) fn schema(value: &Value, context: &mut Context) -> Result<Schema, Error> {
    let path = path(value, context.project_dir);
    let text = read(&path, context)?;
    let tree = parse_json(&text).map_err(|exception| thrown(exception.message))?;
    let json = serde_json::from_str(&text)
        .map_err(|error| thrown(format!("Invalid JSON schema {}: {error}", path.uri())))?;
    Ok(Schema { json, tree })
}

pub(crate) fn read(path: &PathValue, context: &mut Context) -> Result<String, Error> {
    let bytes = context.host.read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// A value as JSON, for validation.
pub(crate) fn json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Boolean(value) => serde_json::Value::Bool(*value),
        Value::Number(Number::Integer(value)) => (*value).into(),
        Value::Number(Number::Long(value)) => (*value).into(),
        Value::Number(number) => serde_json::Number::from_f64(number.to_f64())
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::List(list) => list.borrow().items.iter().map(json).collect(),
        Value::Map(_) => serde_json::Value::Object(
            entries(value)
                .into_iter()
                .map(|(key, value)| (key, json(&value)))
                .collect(),
        ),
        other => serde_json::Value::String(other.to_string()),
    }
}

/// The version of nf-schema that the config asks for, if it names one; else the latest runs.
pub(crate) fn version(context: &Context) -> Option<Vec<u32>> {
    let Some(Value::Set(plugins)) = context.config.get(&["plugins"]) else {
        return None;
    };
    let plugins = plugins.borrow();
    plugins.items.iter().find_map(|id| {
        let id = id.0.to_string();
        let version = id.strip_prefix("nf-schema@")?.to_owned();
        let parts: Vec<u32> = version
            .split('.')
            .map_while(|part| part.parse().ok())
            .collect();
        Some(parts)
    })
}

/// Whether types are checked leniently, as `validation.lenientMode` asks: a `string` then also
/// takes a number, a boolean or null, as nf-schema's `LenientTypeEvaluator` does.
pub(crate) fn lenient(context: &Context) -> bool {
    context
        .config
        .get(&["validation", "lenientMode"])
        .is_some_and(|value| value.truth())
}

/// Returns nf-schema errors by value path; missing fields form one object error. `what` labels
/// the fields; the config may relax type checking. With the schema and the names of the params
/// in the order they were set as `order`, the errors come in the order of nf-schema.
pub(crate) fn errors(
    validator: &jsonschema::Validator,
    instance: &serde_json::Value,
    what: &str,
    context: &Context,
    order: Option<(&Value, &[String])>,
) -> Vec<(String, String)> {
    use jsonschema::error::{TypeKind, ValidationErrorKind as Kind};
    let lenient = lenient(context);
    // From 2.7.3 on, a newer validator words patterns and enums without extra brackets.
    let bare = version(context).is_none_or(|parts| parts.as_slice() >= [2, 7, 3].as_slice());
    // Each error with the schema location that reports it.
    let mut errors: Vec<(String, String, String)> = Vec::new();
    // Missing fields by the value and the `required` keyword that asks for them.
    let mut missing: Vec<(String, String, Vec<String>)> = Vec::new();
    for error in validator.iter_errors(instance) {
        let path = error.instance_path().to_string();
        let location = error.evaluation_path().to_string();
        let value = error.instance();
        let message = match error.kind() {
            Kind::Required { property } => {
                let property = property.as_str().unwrap_or_default().to_owned();
                let group = missing
                    .iter_mut()
                    .find(|(at, by, _)| *at == path && *by == location);
                match group {
                    Some((_, _, names)) => names.push(property),
                    None => missing.push((path, location, vec![property])),
                }
                continue;
            }
            Kind::Type { kind } => {
                let expected: Vec<String> = match kind {
                    TypeKind::Single(kind) => vec![kind.to_string()],
                    TypeKind::Multiple(kinds) => {
                        kinds.iter().map(|kind| kind.to_string()).collect()
                    }
                };
                let scalar = !matches!(
                    &**value,
                    serde_json::Value::Array(_) | serde_json::Value::Object(_)
                );
                if lenient && scalar && expected.iter().any(|kind| kind == "string") {
                    continue;
                }
                let expected = expected.join(", ");
                format!("Value is [{}] but should be [{expected}]", json_type(value))
            }
            Kind::Pattern { pattern } if bare => {
                format!("{value} does not match regular expression {pattern}")
            }
            Kind::Pattern { pattern } => {
                format!("{value} does not match regular expression [{pattern}]")
            }
            Kind::Enum { options } => {
                let options: Vec<String> = options
                    .as_array()
                    .map(|items| items.iter().map(plain).collect())
                    .unwrap_or_default();
                match bare {
                    true => format!("Expected any of [{}]", options.join(", ")),
                    false => format!("Expected any of [[{}]]", options.join(", ")),
                }
            }
            // The wording of nf-schema's validator, which differs from the crate's.
            Kind::Minimum { limit } => format!("{value} is less than {limit}"),
            Kind::Maximum { limit } => format!("{value} is greater than {limit}"),
            Kind::ExclusiveMinimum { limit } => {
                format!("{value} is less than or equal to {limit}")
            }
            Kind::ExclusiveMaximum { limit } if bare => {
                format!("{value} is greater than or equal to {limit}")
            }
            Kind::ExclusiveMaximum { limit } => format!("{value} is greater or equal to {limit}"),
            Kind::MinLength { limit } => format!("{value} is shorter than {limit} characters"),
            Kind::MaxLength { limit } => format!("{value} is longer than {limit} characters"),
            Kind::MultipleOf { multiple_of } => {
                format!("{value} is not multiple of {multiple_of}")
            }
            _ => error.to_string(),
        };
        errors.push((location, path, message));
    }
    for (path, location, names) in missing {
        // The validator collects the missing names in a Java hash set.
        let mut order: Vec<(usize, String)> = (names.iter())
            .map(|name| (hash_position(&names, name), name.clone()))
            .collect();
        order.sort();
        let names: Vec<String> = order.into_iter().map(|(_, name)| name).collect();
        let message = format!("Missing required {what}(s): {}", names.join(", "));
        errors.push((location, path, message));
    }
    if let Some((schema, names)) = order {
        errors.sort_by_cached_key(|(location, _, _)| java_order(schema, names, location));
    }
    errors
        .into_iter()
        .map(|(_, path, message)| (path, message))
        .collect()
}

/// The place of a schema location in the order of nf-schema's validator, which reads the
/// schema into Java hash maps and checks the keywords of each in the order of the map.
/// The params of one `properties` keyword come in the order of the hash map of the params.
fn java_order(schema: &Value, params: &[String], location: &str) -> Vec<usize> {
    let mut node = schema.clone();
    let mut order = Vec::new();
    let mut previous = String::new();
    let mut top = true;
    for segment in location.split('/').skip(1) {
        let segment = segment.replace("~1", "/").replace("~0", "~");
        let next = match &node {
            Value::Map(_) if top && previous == "properties" => {
                top = false;
                order.push(hash_position(params, &segment));
                get(&node, &segment)
            }
            Value::Map(_) => {
                let names: Vec<String> = entries(&node).into_iter().map(|(name, _)| name).collect();
                order.push(hash_position(&names, &segment));
                match (segment.as_str(), get(&node, &segment)) {
                    ("$ref", Some(reference)) => pointer(schema, &reference.to_string()),
                    (_, next) => next,
                }
            }
            Value::List(list) => {
                let index = segment.parse().unwrap_or(usize::MAX);
                order.push(index);
                list.borrow().items.get(index).cloned()
            }
            _ => None,
        };
        match next {
            Some(next) => node = next,
            None => break,
        }
        previous = segment;
    }
    order
}

/// The place of a key in a Java `HashMap` with these keys, put in this order: by the bucket of
/// its hash, then in the order the keys were put.
fn hash_position(names: &[String], name: &str) -> usize {
    let mut capacity = 16;
    while names.len() * 4 > capacity * 3 {
        capacity *= 2;
    }
    let bucket = |name: &str| {
        let hash = name.encode_utf16().fold(0u32, |hash, unit| {
            hash.wrapping_mul(31).wrapping_add(u32::from(unit))
        });
        ((hash ^ (hash >> 16)) as usize) & (capacity - 1)
    };
    let mut order: Vec<usize> = (0..names.len()).collect();
    order.sort_by_key(|index| (bucket(&names[*index]), *index));
    order
        .iter()
        .position(|index| names[*index] == name)
        .unwrap_or(names.len())
}

/// The part of a schema that a local reference like `#/$defs/group` points to.
fn pointer(schema: &Value, reference: &str) -> Option<Value> {
    let path = reference.strip_prefix('#')?;
    let mut node = schema.clone();
    for segment in path.split('/').skip(1) {
        let segment = segment.replace("~1", "/").replace("~0", "~");
        node = match &node {
            Value::List(list) => list
                .borrow()
                .items
                .get(segment.parse::<usize>().ok()?)
                .cloned()?,
            _ => get(&node, &segment)?,
        };
    }
    Some(node)
}

/// The JSON type of a value, as nf-schema names it.
pub(crate) fn json_type(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(number) if number.is_f64() => "number",
        serde_json::Value::Number(_) => "integer",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// A JSON value as Groovy prints it: text without quotes.
pub(crate) fn plain(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// nf-schema's `SchemaValidationException`.
pub(crate) fn thrown(message: String) -> Error {
    Error::Thrown {
        exception: Rc::new(Exception::new("SchemaValidationException", message)),
        span: Default::default(),
    }
}
