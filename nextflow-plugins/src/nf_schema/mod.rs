//! nf-schema: parameter validation and summaries, and samplesheets as lists, from JSON
//! schemas (JSON Schema 2020-12, through the `jsonschema` crate).
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod help;
mod params;
pub(crate) mod paths;
mod samplesheet;
mod settings;
mod summary;

use std::collections::BTreeSet;
use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, parse_json};
use nextflow_evaluator::number::Number;
use nextflow_evaluator::value::{Key, PathValue, State, Value};

pub(crate) use settings::start;

use crate::Context;

pub(crate) fn call(
    name: &str,
    arguments: Vec<Value>,
    context: &mut Context,
) -> Result<Value, Error> {
    // Named arguments come first as a map; nf-schema also takes a map of options as the last
    // argument of `samplesheetToList`. From 3.0.0 on, help takes its param as an option.
    let modern = since(context, [3, 0, 0]);
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
        ("paramsSummaryMap", [workflow]) => summary::map(None, workflow, context),
        ("paramsSummaryMap", [options, workflow]) => summary::map(Some(options), workflow, context),
        ("paramsSummaryLog", [workflow]) => summary::log(None, workflow, context),
        ("paramsSummaryLog", [options, workflow]) => summary::log(Some(options), workflow, context),
        ("paramsHelp", []) => help::text(None, None, context),
        ("paramsHelp", [options @ Value::Map(_)]) => help::text(Some(options), None, context),
        ("paramsHelp", [parameter @ Value::String(_)]) if !modern => {
            help::text(None, Some(parameter.to_string()), context)
        }
        ("paramsHelp", [options @ Value::Map(_), parameter]) if !modern => {
            help::text(Some(options), Some(parameter.to_string()), context)
        }
        ("paramsHelp", _) => Err(no_signature(name, &arguments)),
        _ => Err(Error::Unsupported {
            message: format!(
                "the function `{name}` of the plugin `nf-schema` with these arguments"
            ),
            span: Default::default(),
        }),
    }
}

/// Groovy's `MissingMethodException` for a call whose arguments fit no form of the function.
fn no_signature(name: &str, arguments: &[Value]) -> Error {
    let types: Vec<&str> = arguments.iter().map(Value::class_name).collect();
    let values: Vec<String> = arguments.iter().map(Value::to_string).collect();
    let message = format!(
        "No signature of method: {name}() is applicable for argument types: ({}) values: [{}]",
        types.join(", "),
        values.join(", ")
    );
    Error::Thrown {
        exception: Rc::new(Exception::new("MissingMethodException", message)),
        span: Default::default(),
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
    let size = settings::Settings::read(context).size;
    let schema = load(schema_path, context)?;
    let instance = json(input);
    let check = check(context, &schema);
    let validator = paths::checker(&schema.json, &instance, context, &check)
        .map_err(|error| thrown(format!("Invalid JSON schema: {error}")))?;
    let found = errors(
        &validator,
        &instance,
        &schema.json,
        "object",
        context,
        Order::Crate,
    );
    let mut errors: Vec<String> = found
        .into_iter()
        .map(|(location, message)| {
            let value = (instance.pointer(&location))
                .map(|value| cut(&shown(value), size))
                .unwrap_or_default();
            match location.as_str() {
                "" => format!("({value}): {message}"),
                location => format!("{location} ({value}): {message}"),
            }
        })
        .collect();
    // nf-schema's validator also reports the root `allOf` with the indexes that failed. The
    // crate gives errors of custom keywords their place in the schema, not the way there.
    let branches: Vec<String> = (schema.json.get("allOf").and_then(|all| all.as_array()))
        .into_iter()
        .flatten()
        .map(|branch| {
            let reference = branch.get("$ref").and_then(|reference| reference.as_str());
            let pointer = reference.and_then(|reference| reference.strip_prefix('#'));
            pointer.map_or(String::new(), |pointer| format!("{pointer}/"))
        })
        .collect();
    let failing: BTreeSet<usize> = (validator.iter_errors(&instance))
        .filter_map(|error| {
            let location = error.evaluation_path().to_string();
            let index = location
                .strip_prefix("/allOf/")
                .and_then(|rest| rest.split('/').next().and_then(|index| index.parse().ok()));
            let keyword = error.schema_path().to_string();
            index.or_else(|| {
                (branches.iter())
                    .position(|branch| !branch.is_empty() && keyword.starts_with(branch.as_str()))
            })
        })
        .collect();
    if !failing.is_empty() {
        let indexes: Vec<String> = failing.iter().map(usize::to_string).collect();
        errors.push(format!(
            "({}): Value does not match against the schemas at indexes [{}]",
            cut(&shown(&instance), size),
            indexes.join(", ")
        ));
    }
    if exit && !errors.is_empty() {
        return Err(thrown(format!("{}\n", errors.join("\n"))));
    }
    Ok(Value::list(errors.into_iter().map(Value::string).collect()))
}

/// Whether the selected nf-schema is this version or newer. Without a pin it is 3.0.0, the
/// latest release; 3.1.0 runs only when a config pins it.
pub(crate) fn since(context: &Context, wanted: [u32; 3]) -> bool {
    match version(context) {
        Some(mut parts) => {
            parts.resize(3.max(parts.len()), 0);
            parts.as_slice() >= wanted.as_slice()
        }
        None => wanted <= [3, 0, 0],
    }
}

/// The path a schema name stands for: the name as given from the launch directory when that
/// exists, else from the project directory. From 3.1.0 on, a relative name first tries the
/// folder of the schema that names it, `base`.
pub(crate) fn locate(value: &Value, base: Option<&str>, context: &mut Context) -> PathValue {
    let launch = context.host.launch_dir();
    let given = path(value, &launch);
    let exists = |path: &PathValue, context: &mut Context| {
        matches!(context.host.metadata(path), Ok(Some(_)))
    };
    if let (Some(base), Value::String(name)) = (base, value)
        && since(context, [3, 1, 0])
        && !name.starts_with('/')
        && !name.contains("://")
    {
        let beside = PathValue::parse(name, base);
        if exists(&beside, context) {
            return beside;
        }
    }
    if exists(&given, context) {
        return given;
    }
    path(value, context.project_dir)
}

/// Reads a schema of nf-schema. A file that is not JSON fails as the selected version words it.
pub(crate) fn load(value: &Value, context: &mut Context) -> Result<Schema, Error> {
    let file = locate(value, None, context);
    let text = read(&file, context)?;
    let parsed = parse_json(&text)
        .map_err(|exception| exception.message)
        .and_then(|tree| {
            let json = serde_json::from_str(&text).map_err(|error| error.to_string())?;
            let base = (file.prefix.is_none().then(|| file.parent()).flatten())
                .map(|folder| folder.text().to_owned());
            Ok(Schema { json, tree, base })
        });
    parsed.map_err(|message| match since(context, [3, 0, 0]) {
        true => Error::Thrown {
            exception: Rc::new(Exception::new("JSONException", message)),
            span: Default::default(),
        },
        false => thrown(format!(
            "Failed to load JSON schema ({value}):\n    {message}\n\n"
        )),
    })
}

/// What nf-schema checks besides JSON Schema, for a schema read from `schema`'s folder.
pub(crate) fn check(context: &Context, schema: &Schema) -> paths::Check {
    paths::Check {
        keywords: true,
        newer: since(context, [2, 8, 0]),
        base: schema.base.clone(),
        ..paths::Check::default()
    }
}

/// A text cut to `size` characters as nf-schema's `maxErrValSize` asks: the first and the last
/// half around `...`; -1 keeps it whole.
pub(crate) fn cut(text: &str, size: i64) -> String {
    let characters: Vec<char> = text.chars().collect();
    match usize::try_from(size) {
        Ok(size) if characters.len() > size => {
            let half = size / 2;
            let start: String = characters[..half].iter().collect();
            let end: String = characters[characters.len() - half..].iter().collect();
            format!("{start}...{end}")
        }
        _ => text.to_owned(),
    }
}

/// A value in an error line: text as it is, lists and maps as JSON in Java hash order, as
/// org.json writes them.
pub(crate) fn shown(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Object(map) => {
            let names: Vec<String> = map.keys().cloned().collect();
            let mut order = names.clone();
            order.sort_by_key(|name| hash_position(&names, name));
            let fields: Vec<String> = (order.iter())
                .map(|name| {
                    let key = serde_json::Value::String(name.clone()).to_string();
                    format!("{key}:{}", json_text(&map[name]))
                })
                .collect();
            format!("{{{}}}", fields.join(","))
        }
        serde_json::Value::Array(items) => {
            let items: Vec<String> = items.iter().map(json_text).collect();
            format!("[{}]", items.join(","))
        }
        other => other.to_string(),
    }
}

/// A value inside JSON text: text quoted.
fn json_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(_) => value.to_string(),
        other => shown(other),
    }
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
    /// The folder of a local schema file, where its `schema` keywords look first from 3.1.0.
    pub base: Option<String>,
}

/// Reads a schema. A relative path starts at the project directory, as nf-schema does.
pub(crate) fn schema(value: &Value, context: &mut Context) -> Result<Schema, Error> {
    let path = path(value, context.project_dir);
    let text = read(&path, context)?;
    let tree = parse_json(&text).map_err(|exception| thrown(exception.message))?;
    let json = serde_json::from_str(&text)
        .map_err(|error| thrown(format!("Invalid JSON schema {}: {error}", path.uri())))?;
    Ok(Schema {
        json,
        tree,
        base: None,
    })
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
    schema_version(context.config)
}

pub(crate) fn schema_version(config: &nextflow_config::Config) -> Option<Vec<u32>> {
    let Some(Value::Set(plugins)) = config.get(&["plugins"]) else {
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

/// The order that errors come in.
pub(crate) enum Order<'a> {
    /// The order of the `jsonschema` crate.
    Crate,
    /// The order of nf-schema for params: the schema and the names of the params in the order
    /// they were set.
    Params(&'a Value, &'a [String]),
    /// The root keywords of a samplesheet schema in Java hash order, then the crate's order.
    Sheet(&'a Value),
}

/// Returns nf-schema errors by value path; missing fields form one object error. `what` labels
/// the fields; the config may relax type checking. An `errorMessage` of the schema that fails
/// is added to its errors in brackets.
pub(crate) fn errors(
    validator: &jsonschema::Validator,
    instance: &serde_json::Value,
    schema: &serde_json::Value,
    what: &str,
    context: &Context,
    order: Order,
) -> Vec<(String, String)> {
    use jsonschema::error::{TypeKind, ValidationErrorKind as Kind};
    let lenient = lenient(context);
    // From 2.7.3 on, a newer validator words patterns and enums without extra brackets.
    let bare = version(context).is_none_or(|parts| parts.as_slice() >= [2, 7, 3].as_slice());
    let note = |keyword: &str| {
        let parent = keyword.rsplit_once('/').map_or("", |(parent, _)| parent);
        let note = schema.pointer(parent)?.get("errorMessage")?.as_str()?;
        Some(format!(" ({note})"))
    };
    // Each error with the schema location that reports it.
    let mut errors: Vec<(String, String, String)> = Vec::new();
    // Missing fields by the value and the `required` keyword that asks for them.
    let mut missing: Vec<(String, String, Vec<String>, String)> = Vec::new();
    for error in validator.iter_errors(instance) {
        let path = error.instance_path().to_string();
        let location = error.evaluation_path().to_string();
        let added = note(&error.schema_path().to_string()).unwrap_or_default();
        let value = error.instance();
        let message = match error.kind() {
            Kind::Required { property } => {
                let property = property.as_str().unwrap_or_default().to_owned();
                let group = missing
                    .iter_mut()
                    .find(|(at, by, _, _)| *at == path && *by == location);
                match group {
                    Some((_, _, names, _)) => names.push(property),
                    None => missing.push((path, location, vec![property], added)),
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
        errors.push((location, path, format!("{message}{added}")));
    }
    for (path, location, names, added) in missing {
        // The validator collects the missing names in a Java hash set.
        let mut order: Vec<(usize, String)> = (names.iter())
            .map(|name| (hash_position(&names, name), name.clone()))
            .collect();
        order.sort();
        let names: Vec<String> = order.into_iter().map(|(_, name)| name).collect();
        let message = format!("Missing required {what}(s): {}{added}", names.join(", "));
        errors.push((location, path, message));
    }
    match order {
        Order::Crate => {}
        Order::Params(schema, names) => {
            errors.sort_by_cached_key(|(location, _, _)| java_order(schema, names, location));
        }
        Order::Sheet(schema) => {
            let keys: Vec<String> = entries(schema).into_iter().map(|(key, _)| key).collect();
            errors.sort_by_cached_key(|(location, _, _)| {
                let keyword = location.split('/').nth(1).unwrap_or_default();
                hash_position(&keys, keyword)
            });
        }
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
