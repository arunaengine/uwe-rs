//! `paramsSummaryMap` and `paramsSummaryLog`: the params that differ from the defaults of the
//! parameters schema, by group, then the core options of the run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::Error;
use nextflow_evaluator::value::Value;

use super::params::{groups, parameters_schema};
use super::{entries, get, version};
use crate::Context;

/// `paramsSummaryMap`: the core options of the run, then for each group of the schema the
/// params that differ from their defaults.
pub(super) fn map(
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
pub(super) fn log(
    options: Option<&Value>,
    workflow: &Value,
    context: &mut Context,
) -> Result<Value, Error> {
    let summary = map(options, workflow, context)?;
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
