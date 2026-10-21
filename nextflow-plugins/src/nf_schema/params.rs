//! The parameters of a pipeline against its `nextflow_schema.json`: validation, summaries
//! and help.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use nextflow_evaluator::exception::Exception;

use super::{Schema, entries, errors, get, json, plain, schema, thrown, version};
use crate::Context;

/// Validates params, warns for unknown nonignored names, and checks all known names.
/// Drops errors prefixed by ignored names; command-line text stays text, including `--n 7`.
/// See the crate README for the nf-schema parameter contract.
pub(super) fn validate(options: Option<&Value>, context: &mut Context) -> Result<Value, Error> {
    let schema = parameters_schema(options, context)?;
    let ignored = ignored(context);
    let groups = groups(&schema.tree);
    let mut instance = serde_json::Map::new();
    // The names of the params in the order they were set, which the JSON map loses.
    let mut names = Vec::new();
    let mut unknown = Vec::new();
    // From 2.7.0 on, nf-schema leaves out only null params instead of all false ones.
    let cleans = version(context).is_some_and(|parts| parts.as_slice() < [2, 7, 0].as_slice());
    for (name, value) in entries(&context.params) {
        let value = match cleans {
            true => cleaned(&value),
            false => without_nulls(&value),
        };
        let Some(value) = value else {
            continue;
        };
        let known = groups
            .iter()
            .flat_map(|(_, properties)| properties)
            .any(|(property, _)| *property == name);
        if !known && !ignored.contains(&name) {
            unknown.push(format!("* --{name}: {value}"));
        }
        // As in nf-schema, all params are checked, also those the groups do not name, like
        // the params of a schema whose groups are not below `$defs` or `definitions`.
        names.push(name.clone());
        instance.insert(name, json(&value));
    }
    // From 2.7.0 on, the false help params that nf-schema adds stay in the checked params.
    if !cleans {
        for name in help_names(context) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    if !unknown.is_empty() {
        let message = format!(
            "WARN: The following invalid input values have been detected:\n\n{}\n\n",
            unknown.join("\n")
        );
        context.host.print(&message);
    }
    let mut instance = serde_json::Value::Object(instance);
    if casts(options, context) {
        cast(&mut instance, context.cli_params);
    }
    let validator = super::paths::validator(&schema.json, &instance, context, false)
        .map_err(|error| thrown(format!("Invalid parameters schema: {error}")))?;
    let order = Some((&schema.tree, names.as_slice()));
    let errors: Vec<String> = errors(&validator, &instance, "parameter", context, order)
        .into_iter()
        .map(|(path, message)| {
            let name = path.trim_start_matches('/');
            match instance.get(name) {
                Some(value) => format!("* --{name} ({}): {message}", shown(value)),
                None => format!("* {message}"),
            }
        })
        .filter(|error: &String| {
            !ignored
                .iter()
                .any(|name| error.starts_with(&format!("* --{name}")))
        })
        .collect();
    if errors.is_empty() {
        return Ok(Value::Null);
    }
    Err(thrown(format!(
        "The following invalid input values have been detected:\n\n{}\n\n",
        errors.join("\n")
    )))
}

/// The names of the help params of `validation.help`, with nf-schema's defaults.
fn help_names(context: &Context) -> Vec<String> {
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
        other => plain(other),
    }
}

/// A param as nf-schema checks it from 2.7.0 on: left out if null, with nulls left out of
/// its maps.
fn without_nulls(value: &Value) -> Option<Value> {
    Some(match value {
        Value::Null => return None,
        Value::Map(_) => Value::map(
            entries(value)
                .into_iter()
                .filter_map(|(key, value)| Some((Value::string(key), without_nulls(&value)?))),
        ),
        other => other.clone(),
    })
}

/// Reads the texts of command-line params as Nextflow's legacy parser did, in every map of
/// the params, as nf-schema's JSON converter does.
fn cast(instance: &mut serde_json::Value, names: &[String]) {
    match instance {
        serde_json::Value::Object(entries) => {
            for (name, value) in entries.iter_mut() {
                match value {
                    serde_json::Value::String(text) if names.contains(name) => {
                        *value = typed(text);
                    }
                    other => cast(other, names),
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(|item| cast(item, names)),
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

/// A param as nf-schema's `cleanParameters` leaves it for validation: a value that Groovy
/// counts as false, like `false`, `''` or `[]`, is left out unless it is zero, and a map is
/// cleaned inside.
fn cleaned(value: &Value) -> Option<Value> {
    let zero = matches!(value, Value::Number(number) if number.is_zero());
    if !value.truth() && !zero {
        return None;
    }
    Some(match value {
        Value::Map(_) => Value::map(
            entries(value)
                .into_iter()
                .filter_map(|(key, value)| Some((Value::string(key), cleaned(&value)?))),
        ),
        other => other.clone(),
    })
}

/// `paramsSummaryMap`: the core options of the run, then for each group of the schema the
/// params that differ from their defaults.
pub(super) fn summary(
    options: Option<&Value>,
    workflow: &Value,
    context: &mut Context,
) -> Result<Value, Error> {
    let schema = parameters_schema(options, context)?;
    let mut core = Vec::new();
    for name in ["revision", "runName", "containerEngine", "container"] {
        let value = match name {
            "containerEngine" => engine(context).map(Value::string),
            _ => get(workflow, name),
        };
        if let Some(value) = value.filter(Value::truth) {
            core.push((Value::string(name), value));
        }
    }
    for name in ["launchDir", "workDir", "projectDir", "userName", "profile"] {
        core.push((
            Value::string(name),
            get(workflow, name).unwrap_or(Value::Null),
        ));
    }
    let files = match get(workflow, "configFiles") {
        Some(Value::List(files)) => {
            let names: Vec<String> = files.borrow().items.iter().map(Value::to_string).collect();
            Value::string(names.join(", "))
        }
        _ => Value::string(""),
    };
    core.push((Value::string("configFiles"), files));
    let mut summary = Vec::new();
    for (title, properties) in groups(&schema.tree) {
        let mut changed = Vec::new();
        for (name, definition) in properties {
            let Some(value) = get(&context.params, &name) else {
                continue;
            };
            let text = if let Value::Null = value {
                None
            } else {
                Some(value.to_string())
            };
            let default = get(&definition, "default").map(|default| default.to_string());
            let differs = match (&default, &text) {
                (Some(default), text) => text.as_deref() != Some(default.as_str()),
                (None, Some(text)) => !text.is_empty() && text != "false",
                (None, None) => false,
            };
            if differs {
                let shown = text.map_or(Value::Null, Value::string);
                changed.push((Value::string(name), shown));
            }
        }
        summary.push((Value::string(title), Value::map(changed)));
    }
    summary.push((Value::string("Core Nextflow options"), Value::map(core)));
    Ok(Value::map(summary))
}

/// `paramsSummaryLog`: the summary as text, one group after another.
pub(super) fn summary_log(
    options: Option<&Value>,
    workflow: &Value,
    context: &mut Context,
) -> Result<Value, Error> {
    let summary = summary(options, workflow, context)?;
    let mut groups: Vec<(String, Vec<(String, Value)>)> = entries(&summary)
        .into_iter()
        .map(|(title, group)| (title, flattened(&group)))
        .collect();
    // From 2.7.0 on the log leaves out the containers, which it takes for a map.
    if version(context).is_none_or(|parts| parts.as_slice() >= [2, 7, 0].as_slice())
        && let Some((_, core)) = groups.last_mut()
    {
        if let Some((_, container)) = core.iter().find(|(name, _)| name == "container")
            && !matches!(container, Value::Map(_))
        {
            let message = format!(
                "Cannot cast object '{container}' with class '{}' to class 'java.util.Map'",
                container.java_class_name()
            );
            return Err(Error::Thrown {
                exception: std::rc::Rc::new(Exception::new("ClassCastException", message)),
                span: Default::default(),
            });
        }
        core.retain(|(name, _)| !name.starts_with("container."));
    }
    let width = groups
        .iter()
        .flat_map(|(_, group)| group)
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0);
    let mut output = String::new();
    for (title, params) in groups {
        if params.is_empty() {
            continue;
        }
        output.push_str(&format!("{title}\n"));
        for (name, value) in params {
            output.push_str(&format!("  {name:width$}: {value}\n"));
        }
        output.push('\n');
    }
    output.push_str("!! Only displaying parameters that differ from the pipeline defaults !!\n");
    output.push_str(&"-".repeat(54));
    Ok(Value::string(output))
}

/// The entries of a summary group, with maps inside as `name.key` entries.
fn flattened(group: &Value) -> Vec<(String, Value)> {
    let mut flat = Vec::new();
    for (name, value) in entries(group) {
        match value {
            Value::Map(_) => flat.extend(
                (flattened(&value).into_iter())
                    .map(|(key, value)| (format!("{name}.{key}"), value)),
            ),
            other => flat.push((name, other)),
        }
    }
    flat
}

/// The container engines of Nextflow, in the order it looks for the enabled one.
const ENGINES: [&str; 7] = [
    "docker",
    "podman",
    "sarus",
    "shifter",
    "singularity",
    "apptainer",
    "charliecloud",
];

/// The container engine that the config enables, which `workflow.containerEngine` gives in
/// Nextflow, else none.
fn engine(context: &Context) -> Option<&'static str> {
    ENGINES.into_iter().find(|engine| {
        (context.config.get(&[engine, "enabled"])).is_some_and(|enabled| enabled.truth())
    })
}

/// `paramsHelp`: each group of the schema with its params and their descriptions.
pub(super) fn help(options: Option<&Value>, context: &mut Context) -> Result<Value, Error> {
    let schema = parameters_schema(options, context)?;
    let mut output = String::new();
    for (title, properties) in groups(&schema.tree) {
        output.push_str(&format!("{title}\n"));
        for (name, definition) in properties {
            let kind = get(&definition, "type").map_or(String::new(), |kind| kind.to_string());
            let description =
                get(&definition, "description").map_or(String::new(), |d| d.to_string());
            output.push_str(&format!("  --{name:<30} [{kind}] {description}\n"));
        }
        output.push('\n');
    }
    Ok(Value::string(output))
}

/// The schema of `parametersSchema`, else of `validation.parametersSchema`, else
/// `nextflow_schema.json` of the project.
fn parameters_schema(options: Option<&Value>, context: &mut Context) -> Result<Schema, Error> {
    let path = options
        .and_then(|options| {
            get(options, "parametersSchema").or_else(|| get(options, "parameters_schema"))
        })
        .or_else(|| context.config.get(&["validation", "parametersSchema"]))
        .filter(Value::truth)
        .unwrap_or_else(|| Value::string("nextflow_schema.json"));
    schema(&path, context)
}

/// The params that validation skips: `validation.ignoreParams` and
/// `validation.defaultIgnoreParams`.
fn ignored(context: &Context) -> Vec<String> {
    let mut names = Vec::new();
    for setting in ["ignoreParams", "defaultIgnoreParams"] {
        if let Some(Value::List(list)) = context.config.get(&["validation", setting]) {
            names.extend(list.borrow().items.iter().map(Value::to_string));
        }
    }
    names
}

/// The groups of a parameters schema, by title, in the order of `$defs`, then the params
/// outside any group.
fn groups(schema: &Value) -> Vec<(String, Vec<(String, Value)>)> {
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
