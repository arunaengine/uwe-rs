//! The single rules of Argo's checks of a submitted workflow (workflow/validate/validate.go of
//! v4.1.4): names, arguments, hooks, template types, the inputs a call supplies, and the order of
//! DAG tasks. The module `validate` applies them while it walks the templates.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

use argo_parser::model::{Arguments, Artifact, DagTask, LifecycleHook, Template};
use argo_parser::{Node, Value as Yaml};
use regex::Regex;

use crate::dag::dependencies;
use crate::validate::Supplied;

/// The keys of a template that give its type.
const TYPES: [&str; 10] = [
    "container",
    "containerSet",
    "steps",
    "script",
    "resource",
    "dag",
    "suspend",
    "data",
    "http",
    "plugin",
];

pub(crate) fn node_types(node: &Node) -> usize {
    TYPES
        .iter()
        .filter(|key| node.get(key).is_some_and(|value| value.value != Yaml::Null))
        .count()
}

pub(crate) fn template_types(template: &Template) -> usize {
    [
        template.container.is_some(),
        template.container_set.is_some(),
        !template.steps.is_empty(),
        template.script.is_some(),
        template.resource.is_some(),
        template.dag.is_some(),
        template.suspend.is_some(),
        template.data.is_some(),
        template.http.is_some(),
        template.plugin.is_some(),
    ]
    .into_iter()
    .filter(|&present| present)
    .count()
}

/// Whether a call supplies the inputs of a template, as Argo's `ProcessArgs` checks it: an input
/// needs a value, a default, a value of the call or a ConfigMap to read, and an input artifact
/// that is not optional needs a location or an artifact of the call.
pub(crate) fn process_args(template: &Template, supplied: Supplied) -> Result<(), String> {
    let Supplied::Arguments(arguments) = supplied else {
        return Ok(());
    };
    for input in &template.inputs.parameters {
        let mut value = input.value.is_some() || input.default.is_some();
        let mut from = input.value_from.as_ref();
        if let Some(argument) = arguments
            .parameters
            .iter()
            .find(|argument| argument.name == input.name)
        {
            if argument.value.is_some() {
                value = true;
                from = None;
            } else {
                value = false;
                from = argument.value_from.as_ref();
            }
        }
        let config_map = from.is_some_and(|from| from.config_map_key_ref.is_some());
        if !value && !config_map {
            return Err(format!("inputs.parameters.{} was not supplied", input.name));
        }
    }
    for input in &template.inputs.artifacts {
        let given = arguments
            .artifacts
            .iter()
            .any(|argument| argument.name == input.name);
        if !input.optional && !has_location(input) && !given {
            return Err(format!("inputs.artifacts.{} was not supplied", input.name));
        }
    }
    Ok(())
}

/// Whether an artifact says where it is, as Argo's `HasLocationOrKey`: a raw artifact, or a
/// location whose key field is filled, like the key of an S3 artifact or the URL of an HTTP
/// artifact. An empty `http: {url: ""}` is no location.
fn has_location(artifact: &Artifact) -> bool {
    let filled = |location: &Option<Node>, keys: &[&str]| {
        location.as_ref().is_some_and(|location| {
            keys.iter().any(|key| {
                location
                    .get(key)
                    .is_some_and(|value| !crate::run::scalar_text(value).is_empty())
            })
        })
    };
    artifact.raw.is_some()
        || filled(&artifact.s3, &["key"])
        || filled(&artifact.gcs, &["key"])
        || filled(&artifact.oss, &["key"])
        || filled(&artifact.azure, &["blob"])
        || filled(&artifact.hdfs, &["path", "addresses"])
        || filled(&artifact.git, &["repo"])
        || filled(&artifact.http, &["url"])
        || filled(&artifact.artifactory, &["url"])
        || filled(&artifact.plugin, &["key"])
}

/// The arguments of a workflow, step or task: valid, unique names, and a value for every
/// parameter and a source for every artifact, as Argo's `validateArguments`.
pub(crate) fn arguments(
    prefix: &str,
    arguments: &Arguments,
    allow_empty: bool,
) -> Result<(), String> {
    let parameters: Vec<&str> = arguments
        .parameters
        .iter()
        .map(|parameter| parameter.name.as_str())
        .collect();
    field_names(&parameters, Names::Parameter)
        .map_err(|error| format!("{prefix}parameters{error}"))?;
    let artifacts: Vec<&str> = arguments
        .artifacts
        .iter()
        .map(|artifact| artifact.name.as_str())
        .collect();
    field_names(&artifacts, Names::Parameter)
        .map_err(|error| format!("{prefix}artifacts{error}"))?;
    for parameter in &arguments.parameters {
        let name = &parameter.name;
        if parameter.value.is_none() && parameter.value_from.is_none() && !allow_empty {
            return Err(format!(
                "{prefix}{name}.value or {prefix}{name}.valueFrom is required"
            ));
        }
        if let Some(from) = &parameter.value_from {
            let allowed = from.config_map_key_ref.is_some()
                || !from.event.is_empty()
                || from.supplied.is_some();
            let other = !from.path.is_empty()
                || !from.json_path.is_empty()
                || !from.parameter.is_empty()
                || !from.expression.is_empty();
            if !allowed || other {
                return Err(format!(
                    "{prefix}{name}.valueFrom only allows: default, configMapKeyRef and supplied"
                ));
            }
        }
        if !parameter.enumeration.is_empty() {
            match &parameter.value {
                None if allow_empty => return Ok(()),
                None => return Err(format!("{prefix}{name}.value is required")),
                Some(value) if !parameter.enumeration.contains(value) => {
                    return Err(format!(
                        "{prefix}{name}.value should be present in {prefix}{name}.enum list"
                    ));
                }
                Some(_) => {}
            }
        }
    }
    for artifact in &arguments.artifacts {
        let name = &artifact.name;
        if artifact.from.is_empty()
            && !has_location(artifact)
            && artifact.from_expression.is_empty()
        {
            return Err(format!(
                "{prefix}{name}.from, artifact location, or key is required"
            ));
        }
        if !artifact.from.is_empty() && !artifact.from_expression.is_empty() {
            return Err(format!(
                "{prefix}{name} shouldn't have both `from` and `fromExpression` in Artifact"
            ));
        }
    }
    Ok(())
}

pub(crate) fn hooks(hooks: &BTreeMap<String, LifecycleHook>, prefix: &str) -> Result<(), String> {
    for (name, hook) in hooks {
        if name != "exit" && hook.expression.is_empty() {
            return Err(format!("{prefix}.{name} Expression required"));
        }
    }
    Ok(())
}

pub(crate) fn one_loop(items: &[Node], param: &str, sequence: bool) -> Result<(), String> {
    let loops = [!items.is_empty(), !param.is_empty(), sequence];
    if loops.into_iter().filter(|&used| used).count() > 1 {
        return Err("only one of withItems, withParam, withSequence can be specified".to_owned());
    }
    Ok(())
}

/// A task argument that reads `{{tasks.x...}}` needs `x` among the task's ancestors.
pub(crate) fn argument_dependencies(
    arguments: &Arguments,
    ancestors: &[String],
) -> Result<(), String> {
    let task = |text: &str| {
        text.strip_prefix("{{tasks.")
            .and_then(|rest| rest.split('.').next())
            .map(str::to_owned)
    };
    for parameter in &arguments.parameters {
        let value = parameter.value.as_ref().map(|value| value.0.as_str());
        if let Some(name) = value.and_then(task)
            && !ancestors.contains(&name)
        {
            return Err(format!(
                "missing dependency '{name}' for parameter '{}'",
                parameter.name
            ));
        }
    }
    for artifact in &arguments.artifacts {
        if let Some(name) = task(&artifact.from)
            && !ancestors.contains(&name)
        {
            return Err(format!(
                "missing dependency '{name}' for artifact '{}'",
                artifact.name
            ));
        }
    }
    Ok(())
}

/// Sorts DAG tasks by their dependencies as Argo's `TopologicalSorting` does, which fails on a
/// repeated name, a dependency that is not a task, and a cycle.
pub(crate) fn sort_tasks(tasks: &[DagTask]) -> Result<(), String> {
    let mut waiting: Vec<(String, Vec<String>)> = Vec::new();
    for task in tasks {
        if waiting.iter().any(|(name, _)| name == &task.name) {
            return Err(format!("duplicated nodeName {}", task.name));
        }
        waiting.push((task.name.clone(), dependencies(task, tasks).names));
    }
    for (_, needs) in &waiting {
        if let Some(missing) = needs
            .iter()
            .find(|need| !waiting.iter().any(|(name, _)| name == *need))
        {
            return Err(format!("invalid dependency {missing}"));
        }
    }
    let mut done: HashSet<String> = HashSet::new();
    while done.len() < waiting.len() {
        let ready: Vec<String> = waiting
            .iter()
            .filter(|(name, needs)| {
                !done.contains(name) && needs.iter().all(|need| done.contains(need))
            })
            .map(|(name, _)| name.clone())
            .collect();
        if ready.is_empty() {
            return Err("graph with cycle".to_owned());
        }
        done.extend(ready);
    }
    Ok(())
}

/// The two rules for names: fields like templates, steps and tasks, and parameters and
/// artifacts.
#[derive(Clone, Copy)]
pub(crate) enum Names {
    Field,
    Parameter,
}

/// Names that are present, valid and unique, as Argo's `validateWorkflowFieldNames`. The error
/// starts with the position, like `[0].name is required`.
pub(crate) fn field_names(names: &[&str], rule: Names) -> Result<(), String> {
    let mut seen: HashSet<&str> = HashSet::new();
    for (position, &name) in names.iter().enumerate() {
        if name.is_empty() {
            return Err(format!("[{position}].name is required"));
        }
        let error = match rule {
            Names::Field => invalid_field_name(name),
            Names::Parameter => (!parameter_name().is_match(name)).then(|| {
                "Parameter/Artifact name must consist of alpha-numeric characters, '_' or '-' e.g. my_param_1, MY-PARAM-1".to_owned()
            }),
        };
        if let Some(error) = error {
            return Err(format!("[{position}].name: '{name}' is invalid: {error}"));
        }
        if !seen.insert(name) {
            return Err(format!("[{position}].name '{name}' is not unique"));
        }
    }
    Ok(())
}

pub(crate) fn invalid_field_name(name: &str) -> Option<String> {
    let mut errors = Vec::new();
    if name.len() > 128 {
        errors.push("must be no more than 128 characters".to_owned());
    }
    if !field_name().is_match(name) {
        errors.push("name must consist of alpha-numeric characters or '-', and must start with an alpha-numeric character (e.g. My-name1-2, 123-NAME)".to_owned());
    }
    (!errors.is_empty()).then(|| errors.join(";"))
}

/// A name with its tags replaced by `x`: Argo fills tags in step names with placeholders before
/// it checks them.
pub(crate) fn without_tags(name: &str) -> String {
    static TAG: OnceLock<Regex> = OnceLock::new();
    TAG.get_or_init(|| Regex::new(r"\{\{[^}]*\}\}").unwrap_or_else(|_| unreachable!()))
        .replace_all(name, "x")
        .into_owned()
}

fn field_name() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"^[a-zA-Z0-9][-a-zA-Z0-9]*$").unwrap_or_else(|_| unreachable!())
    })
}

fn parameter_name() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^[-a-zA-Z0-9_]+$").unwrap_or_else(|_| unreachable!()))
}
