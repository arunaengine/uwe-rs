//! The parameters of a pipeline against its `nextflow_schema.json`: validation, summaries
//! and help.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use super::{Schema, entries, errors, get, json, plain, schema, thrown};
use crate::Context;

/// `validateParameters`: checks the params against the schema. Params that the schema does
/// not know give a warning, unless the config ignores them. Ignored params that the schema
/// knows are checked like the others, but as in nf-schema, the errors that start with the
/// name of an ignored param are dropped. Text from the command line stays text, so `--n 7`
/// is not an integer.
pub(super) fn validate(options: Option<&Value>, context: &mut Context) -> Result<Value, Error> {
    let schema = parameters_schema(options, context)?;
    let ignored = ignored(context);
    let groups = groups(&schema.tree);
    let mut instance = serde_json::Map::new();
    let mut unknown = Vec::new();
    for (name, value) in entries(&context.params) {
        let Some(value) = cleaned(&value) else {
            continue;
        };
        let known = groups
            .iter()
            .flat_map(|(_, properties)| properties)
            .any(|(property, _)| *property == name);
        if !known {
            if !ignored.contains(&name) {
                unknown.push(format!("* --{name}: {value}"));
            }
            continue;
        }
        instance.insert(name, json(&value));
    }
    if !unknown.is_empty() {
        let message = format!(
            "WARN: The following invalid input values have been detected:\n\n{}\n\n",
            unknown.join("\n")
        );
        context.host.print(&message);
    }
    let instance = serde_json::Value::Object(instance);
    let validator = super::paths::validator(&schema.json, &instance, context)
        .map_err(|error| thrown(format!("Invalid parameters schema: {error}")))?;
    let errors: Vec<String> = errors(&validator, &instance, "parameter")
        .into_iter()
        .map(|(path, message)| {
            let name = path.trim_start_matches('/');
            match instance.get(name) {
                Some(value) => format!("* --{name} ({}): {message}", plain(value)),
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
        if let Some(value) = get(workflow, name).filter(Value::truth) {
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
    let groups = entries(&summary);
    let width = groups
        .iter()
        .flat_map(|(_, group)| entries(group))
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0);
    let mut output = String::new();
    for (title, group) in groups {
        let params = entries(&group);
        if params.is_empty() {
            continue;
        }
        output.push_str(&format!("{title}\n"));
        for (name, value) in params {
            output.push_str(&format!("  {name:width$}: {value}\n"));
        }
    }
    output.push_str("!! Only displaying parameters that differ from the pipeline defaults !!\n");
    output.push_str(&"-".repeat(54));
    Ok(Value::string(output))
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
