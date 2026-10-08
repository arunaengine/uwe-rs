//! Checks workflow names, arguments, templates and DAG ordering against Argo rules.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

use argo_parser::model::{Arguments, Artifact, DagTask, LifecycleHook, Template};
use argo_parser::{Node, Value as Yaml, parse_yaml};
use regex::Regex;

use crate::dag::dependencies;
use crate::validate::{Base, Supplied};

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
        let config_map = from.is_some_and(|from| from.config_key_ref.is_some());
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

/// Explains unsupported cluster-dependent templates, except locally modeled claims and child
/// workflows.
/// See `docs/design/argo/runtime.md` for the full contract.
pub(crate) fn cluster_only(template: &Template) -> Option<String> {
    let name = &template.name;
    if let Some(node) = &template.synchronization
        && let Err(error) = crate::sync::check(node)
    {
        return Some(error.message);
    }
    for artifact in template
        .inputs
        .artifacts
        .iter()
        .chain(&template.outputs.artifacts)
        .chain(
            template
                .steps
                .iter()
                .flatten()
                .flat_map(|step| &step.arguments.artifacts),
        )
        .chain(
            template
                .dag
                .iter()
                .flat_map(|dag| &dag.tasks)
                .flat_map(|task| &task.arguments.artifacts),
        )
    {
        if let Err(failure) = crate::inputs::check_artifact(artifact) {
            return Some(failure.message);
        }
    }
    for artifact in &template.outputs.artifacts {
        if artifact.s3.is_some()
            || artifact.gcs.is_some()
            || artifact.azure.is_some()
            || artifact.http.is_some()
            || artifact.artifactory.is_some()
        {
            return Some(format!(
                "artifact {}: declared repository uploads are not supported; use the results destination",
                artifact.name
            ));
        }
    }
    if template.resource.is_some() {
        for parameter in &template.outputs.parameters {
            if parameter.value_from.as_ref().is_some_and(|from| {
                !from.jq_filter.is_empty()
                    || !from.expression.is_empty()
                    || !from.parameter.is_empty()
                    || !from.path.is_empty()
            }) {
                return Some(format!(
                    "template {name}: resource output {} supports only valueFrom.jsonPath",
                    parameter.name
                ));
            }
        }
    }
    if template.steps.is_empty() && template.dag.is_none() && template.resource.is_none() {
        for parameter in &template.outputs.parameters {
            if parameter.value_from.as_ref().is_some_and(|from| {
                !from.json_path.is_empty()
                    || !from.jq_filter.is_empty()
                    || !from.expression.is_empty()
                    || !from.parameter.is_empty()
            }) {
                return Some(format!(
                    "template {name}: output {} supports only valueFrom.path",
                    parameter.name
                ));
            }
        }
    }
    for (field, present) in [
        ("suspend", template.suspend.is_some()),
        ("podSpecPatch", template.pod_spec_patch.is_some()),
        ("podResources", template.pod_resources.is_some()),
        ("resourceClaims", template.resource_claims.is_some()),
        ("pendingTimeout", template.pending_timeout.is_some()),
        ("archiveLocation", template.archive_location.is_some()),
    ] {
        if present {
            return Some(format!("template {name}: `{field}` is not supported"));
        }
    }
    for container in template.container.iter().chain(&template.script).chain(
        template
            .container_set
            .iter()
            .flat_map(|set| &set.containers),
    ) {
        for (field, present) in [
            ("lifecycle", container.lifecycle.is_some()),
            ("livenessProbe", container.liveness_probe.is_some()),
            ("startupProbe", container.startup_probe.is_some()),
            ("restartPolicy", container.restart_policy.is_some()),
            (
                "restartPolicyRules",
                container.restart_policy_rules.is_some(),
            ),
            ("resizePolicy", container.resize_policy.is_some()),
        ] {
            if present {
                return Some(format!(
                    "template {name}: container `{field}` is not supported"
                ));
            }
        }
    }
    let reason = if let Some(resource) = &template.resource {
        // Tags that name no variable yet stand for text, so that the manifest parses.
        let kind = parse_yaml(&without_tags(&resource.manifest))
            .ok()
            .flatten()
            .and_then(|manifest| {
                manifest
                    .get("kind")
                    .and_then(Node::as_str)
                    .map(str::to_owned)
            });
        let claim = kind.as_deref() == Some("PersistentVolumeClaim")
            && matches!(resource.action.as_str(), "create" | "apply" | "delete");
        if claim || crate::child::runs_workflow(resource) {
            return None;
        }
        let objects = kind.map_or(String::new(), |kind| format!(" of kind {kind}"));
        format!("is a resource template that manages Kubernetes objects{objects}")
    } else if template.data.is_some() {
        "is a data template, which reads an artifact repository from inside a cluster".to_owned()
    } else if !template.sidecars.is_empty() {
        "has sidecars, services that run beside the task in its Kubernetes pod".to_owned()
    } else if template.daemon == Some(true) {
        "runs as a daemon, a service that later steps reach through the cluster network".to_owned()
    } else {
        return None;
    };
    Some(format!(
        "template {name} {reason}; iunctus runs workflows without Kubernetes and does not support this (decision 18)"
    ))
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

/// The checks of a leaf template: its image, the action and manifest of a resource, and fields
/// that only steps and DAGs may have.
pub(crate) fn leaf_checks(template: &Template, base: &Base) -> Result<(), String> {
    let name = &template.name;
    let default_image = |kind: &str| {
        base.defaults
            .as_ref()
            .and_then(|defaults| defaults.get(kind))
            .and_then(|container| container.get("image"))
            .and_then(Node::as_str)
            .is_some_and(|image| !image.is_empty())
    };
    for (kind, container) in [
        ("container", &template.container),
        ("script", &template.script),
    ] {
        if let Some(container) = container
            && container.image.is_empty()
            && !default_image(kind)
        {
            return Err(format!("templates.{name}.{kind}.image may not be empty"));
        }
    }
    if let Some(set) = &template.container_set {
        let inputs_or_outputs = !template.inputs.artifacts.is_empty()
            || !template.outputs.parameters.is_empty()
            || !template.outputs.artifacts.is_empty();
        if inputs_or_outputs
            && !set
                .containers
                .iter()
                .any(|container| container.name == "main")
        {
            return Err(format!(
                "templates.{name}.containerSet.containers must have a container named \"main\" for input or output"
            ));
        }
    }
    if let Some(resource) = &template.resource {
        let action = resource.action.as_str();
        let known = matches!(
            action,
            "get" | "create" | "apply" | "delete" | "replace" | "patch"
        );
        if !known && !action.contains("{{") {
            return Err(format!(
                "templates.{name}.resource.action must be one of: get, create, apply, delete, replace, patch"
            ));
        }
        if action != "delete" && action != "get" {
            if resource.manifest.is_empty() && resource.manifest_from.is_none() {
                return Err(format!(
                    "either templates.{name}.resource.manifest or templates.{name}.resource.manifestFrom must be specified"
                ));
            }
            if !resource.manifest.is_empty() && resource.manifest_from.is_some() {
                return Err("shouldn't have both `manifest` and `manifestFrom` specified in `Manifest` for resource template".to_owned());
            }
        }
    }
    if template.parallelism.is_some() {
        return Err(format!(
            "templates.{name}.parallelism is only valid for steps and dag templates"
        ));
    }
    Ok(())
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
            let allowed =
                from.config_key_ref.is_some() || !from.event.is_empty() || from.supplied.is_some();
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
