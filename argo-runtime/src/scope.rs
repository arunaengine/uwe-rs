//! The local scope of steps and DAG templates (workflow/controller/scope.go): the inputs of
//! the template and what earlier steps and tasks made, as `steps.<name>.*` and
//! `tasks.<name>.*`, and the outputs that steps and DAG templates take from it.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::PathBuf;

use argo_parser::model::Artifact;

use crate::node::{ArtifactValue, Location, NodeKind, NodeOutputs, Phase};
use crate::run::{Processed, Run};
use crate::tags::Variables;
use crate::time;
use crate::value::Value;

#[derive(Clone, Debug, Default)]
pub(crate) struct Scope {
    pub values: BTreeMap<String, String>,
    pub artifacts: BTreeMap<String, ArtifactValue>,
}

impl Scope {
    /// The inputs of a template, which its steps and tasks can read.
    pub fn of_template(processed: &Processed) -> Scope {
        let mut scope = Scope::default();
        for parameter in &processed.template.inputs.parameters {
            scope.values.insert(
                format!("inputs.parameters.{}", parameter.name),
                parameter.value.clone().unwrap_or_default().0,
            );
        }
        for artifact in &processed.artifacts {
            scope.artifacts.insert(
                format!("inputs.artifacts.{}", artifact.name),
                artifact.clone(),
            );
        }
        scope
    }
}

/// An artifact as expressions see it, from which [`artifact_of`] reads it back.
pub(crate) fn artifact_object(artifact: &ArtifactValue) -> Value {
    let mut map = BTreeMap::new();
    map.insert("name".to_owned(), Value::string(artifact.name.clone()));
    map.insert("path".to_owned(), Value::string(artifact.path.clone()));
    let (kind, location) = match &artifact.location {
        Some(Location::File(path)) => ("file", path.to_string_lossy().into_owned()),
        Some(Location::Url(url)) => ("url", url.clone()),
        Some(Location::Raw(data)) => ("raw", data.clone()),
        None => ("none", String::new()),
    };
    map.insert("$kind".to_owned(), Value::string(kind));
    map.insert("$location".to_owned(), Value::string(location));
    map.insert("$directory".to_owned(), Value::Bool(artifact.directory));
    Value::Map(map)
}

pub(crate) fn artifact_of(value: &Value) -> Option<ArtifactValue> {
    let Value::Map(map) = value else {
        return None;
    };
    let text = |key: &str| {
        map.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let location = match text("$kind").as_str() {
        "file" => Some(Location::File(PathBuf::from(text("$location")))),
        "url" => Some(Location::Url(text("$location"))),
        "raw" => Some(Location::Raw(text("$location"))),
        "none" => None,
        _ => return None,
    };
    Some(ArtifactValue {
        name: text("name"),
        path: text("path"),
        location,
        optional: false,
        directory: map.get("$directory") == Some(&Value::Bool(true)),
    })
}

impl Run {
    /// The variables of a phase that sees the globals and a local scope.
    pub fn variables(&self, scope: &Scope) -> Variables {
        let mut values = self.globals.clone();
        values.extend(scope.values.clone());
        let mut variables = Variables::new(values);
        for (name, artifact) in self.global_artifacts.iter().chain(&scope.artifacts) {
            variables = variables.with_object(name.clone(), artifact_object(artifact));
        }
        variables
    }

    /// Adds what a node made to a scope under a prefix like `steps.hello`, as Argo's
    /// `buildLocalScope` does. A retry node gives what its last attempt made.
    pub fn add_node_scope(&self, scope: &mut Scope, prefix: &str, index: usize) {
        let mut node = &self.nodes[index];
        if node.kind == NodeKind::Retry
            && let Some(&last) = node.children.last()
        {
            node = &self.nodes[last];
        }
        let values = &mut scope.values;
        values.insert(format!("{prefix}.id"), node.id.clone());
        values.insert(format!("{prefix}.status"), node.phase.name().to_owned());
        values.insert(format!("{prefix}.startedAt"), time::rfc3339(node.started));
        if let Some(finished) = node.finished {
            values.insert(format!("{prefix}.finishedAt"), time::rfc3339(finished));
        }
        if node.kind == NodeKind::Pod {
            values.insert(format!("{prefix}.ip"), crate::run::pod_ip(&node.name));
            values.insert(format!("{prefix}.hostNodeName"), "iunctus".to_owned());
        }
        add_outputs(scope, prefix, &node.outputs);
    }

    /// Adds the outputs of the iterations of a loop as JSON lists, as Argo's
    /// `processAggregateNodeOutputs` does.
    pub fn add_aggregate_scope(&self, scope: &mut Scope, prefix: &str, children: &[usize]) {
        if children.is_empty() {
            return;
        }
        let mut children: Vec<usize> = children.to_vec();
        children.sort_by_key(|&child| loop_index(&self.nodes[child].name));
        let mut results = Vec::new();
        let mut parameter_maps = Vec::new();
        let mut lists: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for child in children {
            let mut node = &self.nodes[child];
            if node.kind == NodeKind::Retry
                && let Some(&last) = node.children.last()
            {
                node = &self.nodes[last];
            }
            if node.phase != Phase::Succeeded {
                continue;
            }
            if !node.outputs.parameters.is_empty() {
                let mut map = BTreeMap::new();
                for (name, value) in &node.outputs.parameters {
                    map.insert(name.clone(), Value::string(value.clone()));
                    lists
                        .entry(name.clone())
                        .or_default()
                        .push(json_or_text(value));
                }
                parameter_maps.push(Value::Map(map));
            }
            if let Some(result) = &node.outputs.result {
                results.push(json_or_text(result));
            }
        }
        scope.values.insert(
            format!("{prefix}.outputs.result"),
            Value::Array(results).to_json(),
        );
        scope.values.insert(
            format!("{prefix}.outputs.parameters"),
            Value::Array(parameter_maps).to_json(),
        );
        for (name, list) in lists {
            scope.values.insert(
                format!("{prefix}.outputs.parameters.{name}"),
                Value::Array(list).to_json(),
            );
        }
    }

    /// The outputs of a steps or DAG template, taken from its scope.
    pub fn outputs_from_scope(
        &mut self,
        processed: &Processed,
        scope: &Scope,
    ) -> Result<NodeOutputs, String> {
        let mut outputs = NodeOutputs::default();
        for parameter in &processed.template.outputs.parameters {
            let Some(from) = &parameter.value_from else {
                match &parameter.value {
                    Some(value) => {
                        outputs
                            .parameters
                            .push((parameter.name.clone(), value.0.clone()));
                        continue;
                    }
                    None => {
                        return Err(format!(
                            "output parameter {} must have a valueFrom",
                            parameter.name
                        ));
                    }
                }
            };
            let resolved = if !from.expression.is_empty() {
                let variables = Variables::new(scope.values.clone());
                match crate::expr::evaluate(&from.expression, variables.environment()) {
                    Ok(Value::Nil) => Err(format!(
                        "the expression of output parameter {} gave nil",
                        parameter.name
                    )),
                    Ok(value) => Ok(value.to_go_string()),
                    Err(error) => Err(error.message),
                }
            } else if !from.parameter.is_empty() {
                let name = from
                    .parameter
                    .trim()
                    .trim_start_matches("{{")
                    .trim_end_matches("}}")
                    .trim();
                match scope.values.get(name) {
                    Some(value) => Ok(value.clone()),
                    // A value that substitution resolved already is taken as it is.
                    None if !crate::tags::has_tag(&from.parameter) => Ok(from.parameter.clone()),
                    None => Err(format!("unable to resolve: {{{{{name}}}}}")),
                }
            } else {
                Err(format!("output parameter {} has no source", parameter.name))
            };
            let source = if from.expression.is_empty() {
                &from.parameter
            } else {
                &from.expression
            };
            let value = match (resolved, &from.default) {
                (Ok(value), _) => value,
                (Err(_), Some(default)) => default.0.clone(),
                (Err(error), None) => match skipped_reference(scope, source) {
                    Some(step) => {
                        self.warn(format!(
                            "output parameter {} reads {step}, whose outputs are not known; it is empty",
                            parameter.name
                        ));
                        String::new()
                    }
                    None => return Err(error),
                },
            };
            if !parameter.global_name.is_empty() {
                self.globals.insert(
                    format!("workflow.outputs.parameters.{}", parameter.global_name),
                    value.clone(),
                );
            }
            outputs.parameters.push((parameter.name.clone(), value));
        }
        for artifact in &processed.template.outputs.artifacts {
            match self.resolve_artifact(artifact, scope) {
                Ok(Some(mut value)) => {
                    value.name = artifact.name.clone();
                    if !artifact.global_name.is_empty() {
                        self.global_artifacts.insert(
                            format!("workflow.outputs.artifacts.{}", artifact.global_name),
                            value.clone(),
                        );
                    }
                    outputs.artifacts.push(value);
                }
                Ok(None) => {}
                Err(_) if artifact.optional => {}
                Err(error) => {
                    let source = if artifact.from_expression.is_empty() {
                        &artifact.from
                    } else {
                        &artifact.from_expression
                    };
                    match skipped_reference(scope, source) {
                        Some(step) => self.warn(format!(
                            "output artifact {} reads {step}, whose outputs are not known; it is left out",
                            artifact.name
                        )),
                        None => return Err(format!("unable to resolve outputs from scope: {error}")),
                    }
                }
            }
        }
        Ok(outputs)
    }

    /// An artifact argument: an artifact that `from` or `fromExpression` names in the scope,
    /// or one with its own location. `None` for an optional artifact that is not there.
    pub fn argument_artifact(
        &mut self,
        artifact: &Artifact,
        scope: &Scope,
    ) -> Result<Option<ArtifactValue>, String> {
        if artifact.from.is_empty() && artifact.from_expression.is_empty() {
            let value = self
                .artifact_value(artifact)
                .map_err(|failure| failure.message)?;
            return Ok(Some(value));
        }
        match self.resolve_artifact(artifact, scope) {
            Ok(Some(mut value)) => {
                value.name = artifact.name.clone();
                Ok(Some(value))
            }
            Ok(None) => Ok(None),
            Err(_) if artifact.optional => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The artifact that `from` or `fromExpression` names, with `subPath` appended.
    fn resolve_artifact(
        &mut self,
        artifact: &Artifact,
        scope: &Scope,
    ) -> Result<Option<ArtifactValue>, String> {
        let found = if !artifact.from_expression.is_empty() {
            let mut variables = Variables::new(scope.values.clone());
            for (name, value) in &scope.artifacts {
                variables = variables.with_object(name.clone(), artifact_object(value));
            }
            let value = crate::expr::evaluate(&artifact.from_expression, variables.environment())
                .map_err(|error| error.message)?;
            artifact_of(&value).ok_or_else(|| {
                format!(
                    "fromExpression `{}` does not give an artifact",
                    artifact.from_expression
                )
            })?
        } else if !artifact.from.is_empty() {
            let name = artifact
                .from
                .trim()
                .trim_start_matches("{{")
                .trim_end_matches("}}")
                .trim();
            match scope
                .artifacts
                .get(name)
                .or_else(|| self.global_artifacts.get(name))
            {
                Some(value) => value.clone(),
                None => return Err(format!("unable to resolve: {{{{{name}}}}}")),
            }
        } else {
            return Ok(None);
        };
        let mut found = found;
        if !artifact.sub_path.is_empty()
            && let Some(location) = &mut found.location
        {
            match location {
                Location::File(path) => *path = path.join(&artifact.sub_path),
                Location::Url(url) => {
                    *url = format!("{}/{}", url.trim_end_matches('/'), artifact.sub_path)
                }
                Location::Raw(_) => {}
            }
        }
        Ok(Some(found))
    }
}

/// Adds outputs under a prefix: `<prefix>.outputs.result`, `.exitCode`, parameters and
/// artifacts.
pub(crate) fn add_outputs(scope: &mut Scope, prefix: &str, outputs: &NodeOutputs) {
    if let Some(result) = &outputs.result {
        scope
            .values
            .insert(format!("{prefix}.outputs.result"), result.clone());
    }
    if let Some(exit_code) = &outputs.exit_code {
        scope
            .values
            .insert(format!("{prefix}.exitCode"), exit_code.clone());
    }
    for (name, value) in &outputs.parameters {
        scope
            .values
            .insert(format!("{prefix}.outputs.parameters.{name}"), value.clone());
    }
    for artifact in &outputs.artifacts {
        scope.artifacts.insert(
            format!("{prefix}.outputs.artifacts.{}", artifact.name),
            artifact.clone(),
        );
    }
}

/// A step or task that a reference or expression reads and whose outputs are not known: it
/// was skipped or omitted, like both branches of a condition that a run with empty outputs
/// decides neither way, or it left an output empty, as a simulated run does.
fn skipped_reference(scope: &Scope, source: &str) -> Option<String> {
    let reads = |kind: &str, step: &str| {
        [
            format!("{kind}.{step}."),
            format!("{kind}['{step}']"),
            format!("{kind}[\"{step}\"]"),
        ]
        .iter()
        .any(|reference| source.contains(reference.as_str()))
    };
    scope.values.iter().find_map(|(key, value)| {
        let (kind, rest) = key.split_once('.')?;
        if kind != "steps" && kind != "tasks" {
            return None;
        }
        let (step, field) = match rest.strip_suffix(".status") {
            Some(step) => (step, "status"),
            None => rest.split_once(".outputs.")?,
        };
        let unknown = match field {
            "status" => value == "Skipped" || value == "Omitted",
            _ => value.is_empty(),
        };
        (unknown && reads(kind, step)).then(|| format!("{kind}.{step}"))
    })
}

/// A value that is JSON as the JSON value, or else as text, as Argo aggregates loop outputs.
fn json_or_text(text: &str) -> Value {
    Value::parse_json(text).unwrap_or_else(|_| Value::string(text))
}

/// The iteration number of a loop node name like `a.b(3:x)`.
pub(crate) fn loop_index(name: &str) -> usize {
    name.rfind('(')
        .and_then(|start| {
            let rest = &name[start + 1..];
            rest.split(':').next()?.parse().ok()
        })
        .unwrap_or(0)
}
