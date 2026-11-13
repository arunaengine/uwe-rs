//! The parameters of a pipeline against its `nextflow_schema.json`: validation, unknown
//! params and the groups of the schema.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use super::settings::{Settings, report};
use super::{Order, Schema, cut, entries, errors, get, json, load, since, thrown, version};
use crate::Context;

/// Validates params, then, when they are valid, reports the params that the schema does not
/// evaluate. Drops errors prefixed by ignored names; command-line text stays text before 2.7.2.
/// See the crate README for the nf-schema parameter contract.
pub(super) fn validate(options: Option<&Value>, context: &mut Context) -> Result<Value, Error> {
    let settings = Settings::read(context);
    let schema = parameters_schema(options, context)?;
    let ignored = &settings.ignored;
    let mut instance = serde_json::Map::new();
    // The names of the params in the order they were set, which the JSON map loses.
    let mut names = Vec::new();
    // From 2.7.0 on, nf-schema leaves out only null params instead of all false ones.
    let cleans = version(context).is_some_and(|parts| parts.as_slice() < [2, 7, 0].as_slice());
    for (name, value) in entries(&context.params) {
        let Some(value) = crate::clean_value(&value, cleans) else {
            continue;
        };
        names.push(name.clone());
        instance.insert(name, json(&value));
    }
    let help = help_names(context);
    // From 2.7.0 on, the false help params that nf-schema adds stay in the checked params.
    if !cleans {
        for name in &help {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
    }
    let mut instance = serde_json::Value::Object(instance);
    if casts(options, context) {
        cast(&mut instance, context.cli_params, since(context, [3, 1, 0]));
    }
    let check = super::check(context, &schema);
    let validator = super::paths::checker(&schema.json, &instance, context, &check)
        .map_err(|error| thrown(format!("Invalid parameters schema: {error}")))?;
    let order = Order::Params(&schema.tree, names.as_slice());
    let found = errors(
        &validator,
        &instance,
        &schema.json,
        "parameter",
        context,
        order,
    );
    let errors: Vec<String> = found
        .into_iter()
        .map(|(path, message)| match instance.pointer(&path) {
            Some(value) if !path.is_empty() => {
                let value = cut(&shown(value), settings.size);
                format!("* --{} ({value}): {message}", dotted(&path))
            }
            _ => format!("* {message}"),
        })
        .filter(|error: &String| {
            !ignored
                .iter()
                .any(|name| error.starts_with(&format!("* --{name}")))
        })
        .collect();
    if !errors.is_empty() {
        context
            .host
            .print("ERROR ~ Validation of pipeline parameters failed!\n");
        return Err(thrown(invalid(&errors)));
    }
    let mut unknown = Vec::new();
    unevaluated(&schema.json, &[&schema.json], &instance, "", &mut unknown);
    unknown.sort_by_key(|name| {
        let top = name.split('.').next().unwrap_or_default();
        names.iter().position(|known| known == top)
    });
    unknown.retain(|name| {
        let top = name.split('.').next().unwrap_or_default();
        !ignored
            .iter()
            .any(|ignored| ignored == name || ignored == top)
            && !help.contains(name)
    });
    let core: Vec<String> = (unknown.iter())
        .filter(|name| CORE_OPTIONS.contains(&name.as_str()))
        .map(|name| {
            format!(
                "* You used a core Nextflow option with two hyphens: '--{name}'. Please \
                 resubmit with '-{name}'"
            )
        })
        .collect();
    if !core.is_empty() {
        return Err(thrown(invalid(&core)));
    }
    if !unknown.is_empty() {
        let lines: Vec<String> = (unknown.iter())
            .map(|name| format!("* --{name}: {}", lookup(&context.params, name)))
            .collect();
        report(context, settings.params_level, &invalid(&lines))?;
    }
    Ok(Value::Null)
}

/// The text of nf-schema's report of invalid params.
fn invalid(lines: &[String]) -> String {
    format!(
        "The following invalid input values have been detected:\n\n{}\n\n",
        lines.join("\n")
    )
}

/// A JSON pointer as a param name with dots, like `tool.level`.
fn dotted(pointer: &str) -> String {
    let parts: Vec<String> = (pointer.split('/').skip(1))
        .map(|part| part.replace("~1", "/").replace("~0", "~"))
        .collect();
    parts.join(".")
}

/// The value of a param by its dotted name.
fn lookup(params: &Value, name: &str) -> Value {
    let mut value = params.clone();
    for part in name.split('.') {
        value = get(&value, part).unwrap_or(Value::Null);
    }
    value
}

/// The run options of `nextflow run`, which a user may give with two hyphens by mistake.
const CORE_OPTIONS: &[&str] = &[
    "ansi-log",
    "bg",
    "bucket-dir",
    "cache",
    "disable-jobs-cancellation",
    "dump-channels",
    "dump-hashes",
    "entry",
    "hub",
    "latest",
    "lib",
    "main-script",
    "name",
    "offline",
    "output-dir",
    "params-file",
    "plugins",
    "preview",
    "profile",
    "queue-size",
    "resume",
    "revision",
    "stub",
    "stub-run",
    "test",
    "user",
    "with-apptainer",
    "with-charliecloud",
    "with-conda",
    "with-dag",
    "with-docker",
    "with-mpi",
    "with-notification",
    "with-podman",
    "with-report",
    "with-singularity",
    "with-spack",
    "with-timeline",
    "with-tower",
    "with-trace",
    "with-wave",
    "with-weblog",
    "without-conda",
    "without-docker",
    "without-podman",
    "without-spack",
    "without-wave",
    "work-dir",
];

/// The names, with dots for nested ones, of the fields of a value that no keyword of its
/// schemas evaluates. A nested value is looked at only where its schemas name properties.
fn unevaluated(
    root: &serde_json::Value,
    schemas: &[&serde_json::Value],
    instance: &serde_json::Value,
    prefix: &str,
    found: &mut Vec<String>,
) {
    let serde_json::Value::Object(values) = instance else {
        return;
    };
    let mut known = Vec::new();
    let mut open = false;
    for schema in schemas {
        evaluated(root, schema, &mut known, &mut open, 0);
    }
    if open || known.is_empty() && !prefix.is_empty() {
        return;
    }
    for (key, value) in values {
        let matching: Vec<&serde_json::Value> = (known.iter())
            .filter(|(name, _)| *name == key)
            .map(|(_, schema)| *schema)
            .collect();
        match matching.is_empty() {
            true => found.push(format!("{prefix}{key}")),
            false => unevaluated(root, &matching, value, &format!("{prefix}{key}."), found),
        }
    }
}

/// Adds the properties that a schema and its subschemas evaluate; `open` when it takes any.
fn evaluated<'a>(
    root: &'a serde_json::Value,
    schema: &'a serde_json::Value,
    known: &mut Vec<(&'a str, &'a serde_json::Value)>,
    open: &mut bool,
    depth: usize,
) {
    let serde_json::Value::Object(map) = schema else {
        *open |= schema.as_bool() == Some(true) && depth > 0;
        return;
    };
    if depth > 32 {
        return;
    }
    if let Some(serde_json::Value::Object(properties)) = map.get("properties") {
        known.extend(
            properties
                .iter()
                .map(|(name, schema)| (name.as_str(), schema)),
        );
    }
    let takes_rest = |keyword: &str| map.get(keyword).is_some_and(|value| value != false);
    if map.contains_key("patternProperties")
        || takes_rest("additionalProperties")
        || takes_rest("unevaluatedProperties")
    {
        *open = true;
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        for schema in map
            .get(keyword)
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
        {
            evaluated(root, schema, known, open, depth + 1);
        }
    }
    for keyword in ["then", "else"] {
        if let Some(schema) = map.get(keyword) {
            evaluated(root, schema, known, open, depth + 1);
        }
    }
    if let Some(serde_json::Value::Object(dependent)) = map.get("dependentSchemas") {
        for schema in dependent.values() {
            evaluated(root, schema, known, open, depth + 1);
        }
    }
    let target = (map.get("$ref").and_then(|value| value.as_str()))
        .and_then(|reference| reference.strip_prefix('#'))
        .and_then(
            |fragment| match fragment.is_empty() || fragment.starts_with('/') {
                true => root.pointer(fragment),
                false => anchored(root, fragment),
            },
        );
    if let Some(target) = target {
        evaluated(root, target, known, open, depth + 1);
    }
}

/// The subschema of a document whose `$anchor` is this name.
fn anchored<'a>(value: &'a serde_json::Value, name: &str) -> Option<&'a serde_json::Value> {
    match value {
        serde_json::Value::Object(map)
            if map.get("$anchor").and_then(|it| it.as_str()) == Some(name) =>
        {
            Some(value)
        }
        serde_json::Value::Object(map) => map.values().find_map(|value| anchored(value, name)),
        serde_json::Value::Array(items) => items.iter().find_map(|value| anchored(value, name)),
        _ => None,
    }
}

/// The names of the help params of `validation.help`, with nf-schema's defaults.
pub(super) fn help_names(context: &Context) -> Vec<String> {
    let defaults = [
        ("shortParameter", "help"),
        ("fullParameter", "helpFull"),
        ("showHiddenParameter", "showHidden"),
    ];
    defaults
        .into_iter()
        .map(|(key, default)| {
            let value = context.config.get(&["validation", "help", key]);
            value.map_or(default.to_owned(), |value| value.to_string())
        })
        .collect()
}

/// Whether the check reads command-line texts as numbers and booleans: the `cast_cli_params`
/// option, else from nf-schema 2.7.2 on, which does so because Nextflow keeps them as text.
/// With `NXF_SYNTAX_PARSER=v1` it does not, as Nextflow reads them as types itself there.
fn casts(options: Option<&Value>, context: &Context) -> bool {
    if let Some(cast) = options.and_then(|options| get(options, "cast_cli_params")) {
        return cast.truth();
    }
    let legacy = context.host.environment("NXF_SYNTAX_PARSER").as_deref() == Some("v1");
    !legacy && version(context).is_none_or(|parts| parts.as_slice() >= [2, 7, 2].as_slice())
}

/// A value in an error line as nf-schema shows it: Groovy's `value ?: ''`, so `false`, zero
/// and empty text show as nothing, but lists and maps, which are JSON there, as JSON.
fn shown(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Bool(false) => String::new(),
        serde_json::Value::Number(number) if number.as_f64() == Some(0.0) => String::new(),
        other => super::shown(other),
    }
}

/// Reads the texts of command-line params as Nextflow's legacy parser did, in every map of
/// the params, as nf-schema's JSON converter does. With `nested`, as from 3.1.0 on, all texts
/// inside a map of the command line are read too, like `--tool.level 9`.
fn cast(instance: &mut serde_json::Value, names: &[String], nested: bool) {
    match instance {
        serde_json::Value::Object(entries) => {
            for (name, value) in entries.iter_mut() {
                match value {
                    serde_json::Value::String(text) if names.contains(name) => {
                        *value = typed(text);
                    }
                    serde_json::Value::Object(_) if nested && names.contains(name) => {
                        cast_all(value);
                    }
                    other => cast(other, names, nested),
                }
            }
        }
        serde_json::Value::Array(items) => {
            items.iter_mut().for_each(|item| cast(item, names, nested));
        }
        _ => {}
    }
}

fn cast_all(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => *value = typed(text),
        serde_json::Value::Object(entries) => entries.values_mut().for_each(cast_all),
        serde_json::Value::Array(items) => items.iter_mut().for_each(cast_all),
        _ => {}
    }
}

/// nf-schema's `parseParamValue`: `true` and `false` in any case, then whole and decimal
/// numbers as Integer, Long or Double, else the text.
fn typed(text: &str) -> serde_json::Value {
    match text.to_lowercase().as_str() {
        "true" => return true.into(),
        "false" => return false.into(),
        _ => {}
    }
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    let unsigned = text.strip_prefix('-').unwrap_or(text);
    let numeric = match unsigned.split_once('.') {
        Some((whole, fraction)) => digits(whole) && digits(fraction),
        None => digits(unsigned),
    };
    if numeric {
        if let Ok(value) = text.parse::<i32>() {
            return value.into();
        }
        if let Ok(value) = text.parse::<i64>() {
            return value.into();
        }
        if let Some(value) = text
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
        {
            return serde_json::Value::Number(value);
        }
    }
    serde_json::Value::String(text.to_owned())
}

/// The schema of `parametersSchema`, else of `validation.parametersSchema`, else
/// `nextflow_schema.json` of the project.
pub(super) fn parameters_schema(
    options: Option<&Value>,
    context: &mut Context,
) -> Result<Schema, Error> {
    let path = options
        .and_then(|options| {
            get(options, "parametersSchema").or_else(|| get(options, "parameters_schema"))
        })
        .or_else(|| context.config.get(&["validation", "parametersSchema"]))
        .filter(Value::truth)
        .unwrap_or_else(|| Value::string("nextflow_schema.json"));
    load(&path, context)
}

/// The groups of a parameters schema, by title, in the order of `$defs`, then the params
/// outside any group.
pub(super) fn groups(schema: &Value) -> Vec<(String, Vec<(String, Value)>)> {
    let mut groups = Vec::new();
    let definitions = get(schema, "$defs").or_else(|| get(schema, "definitions"));
    for (name, group) in definitions.map_or_else(Vec::new, |d| entries(&d)) {
        let title = get(&group, "title").map_or(name, |title| title.to_string());
        let properties = get(&group, "properties").map_or_else(Vec::new, |p| entries(&p));
        groups.push((title, properties));
    }
    if let Some(properties) = get(schema, "properties") {
        groups.push(("Other parameters".into(), entries(&properties)));
    }
    groups
}

#[cfg(test)]
mod tests {
    /// A property behind a `$ref` to an `$anchor` is known, as the validator sees it.
    #[test]
    fn follows_anchors() {
        let schema = serde_json::json!({
            "$defs": {"inputs": {"$anchor": "inputs", "properties": {"input": {}}}},
            "allOf": [{"$ref": "#inputs"}],
        });
        let instance = serde_json::json!({"input": "a.csv", "other": 1});
        let mut found = Vec::new();
        super::unevaluated(&schema, &[&schema], &instance, "", &mut found);
        assert_eq!(found, ["other"]);
    }
}
