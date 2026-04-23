//! Arguments into inputs, as Argo's `ProcessArgs` and `SubstituteParams` do: the values of input
//! parameters, the substitution of a template in two phases, and where artifacts come from.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::rc::Rc;

use argo_parser::model::{Artifact, Template};
use argo_parser::{Entry, Node, Value as Yaml, decode};

use crate::node::{ArtifactValue, Location};
use crate::run::{
    Args, Context, Failure, Processed, Result, Run, entry_mut, is_pod, parameters_json,
    placeholder, scalar_text,
};
use crate::tags::{Variables, replace_node};

impl Run {
    /// Applies the arguments to a template and substitutes its tags, as Argo's `ProcessArgs`
    /// and `SubstituteParams` do: globals first, then the inputs.
    pub(crate) fn process_args(
        &mut self,
        name: &str,
        template: &Node,
        context: &Rc<Context>,
        args: &Args,
        locals: Option<BTreeMap<String, String>>,
    ) -> Result<Processed> {
        let (typed, _) = decode::<Template>(template).map_err(|error| {
            Failure::error(format!("template {}: {error}", template_label(template)))
        })?;
        let mut node = template.clone();
        let mut values: Vec<(String, String)> = Vec::new();
        for (position, input) in typed.inputs.parameters.iter().enumerate() {
            let mut value = input.value.clone().or_else(|| input.default.clone());
            let mut from_config_map = input
                .value_from
                .as_ref()
                .is_some_and(|from| from.config_map_key_ref.is_some());
            if let Some(argument) = args
                .parameters
                .iter()
                .find(|parameter| parameter.name == input.name)
            {
                if let Some(given) = &argument.value {
                    value = Some(given.clone());
                    from_config_map = false;
                } else if let Some(from) = &argument.value_from {
                    if from.config_map_key_ref.is_some() {
                        from_config_map = true;
                        value = from.default.clone().or(value);
                    } else if !from.supplied.is_none() || !from.expression.is_empty() {
                        value = from.default.clone().or(value);
                    }
                }
            }
            if from_config_map {
                let fallback = input
                    .value_from
                    .as_ref()
                    .and_then(|from| from.default.clone());
                value = value.or(fallback);
                self.warn(format!(
                    "input {} of template {} reads a ConfigMap, which a run without Kubernetes cannot",
                    input.name, typed.name
                ));
                if value.is_none() {
                    value = Some(placeholder(&input.name));
                }
            }
            let Some(value) = value else {
                return Err(Failure::error(format!(
                    "inputs.parameters.{} was not supplied",
                    input.name
                )));
            };
            set_input_value(&mut node, position, &value.0);
            values.push((input.name.clone(), value.0));
        }
        let mut artifacts = Vec::new();
        for input in &typed.inputs.artifacts {
            let argument = args
                .artifacts
                .iter()
                .find(|artifact| artifact.name == input.name);
            let own = self.artifact_value(input)?;
            let mut value = match argument {
                Some(argument) if argument.location.is_some() => argument.clone(),
                _ => own,
            };
            if value.location.is_none() && !input.optional {
                return Err(Failure::error(format!(
                    "inputs.artifacts.{} was not supplied",
                    input.name
                )));
            }
            value.name = input.name.clone();
            value.path = input.path.clone();
            value.optional = input.optional;
            artifacts.push(value);
        }
        let mut locals = locals.unwrap_or_default();
        if is_pod(&typed) {
            locals
                .entry("pod.name".to_owned())
                .or_insert_with(|| self.pod_name(name, &typed.name));
        }
        locals.insert("node.name".to_owned(), name.to_owned());
        let mut first = self.globals.clone();
        first.extend(locals.clone());
        let variables = Variables::new(first.clone());
        replace_node(&mut node, &variables, true).map_err(Failure::error)?;
        // The inputs as the first pass left them, for the second pass.
        let (after, _) = decode::<Template>(&node)
            .map_err(|error| Failure::error(format!("template {}: {error}", typed.name)))?;
        let mut second = first;
        for input in &after.inputs.parameters {
            second.insert(
                format!("inputs.parameters.{}", input.name),
                input.value.clone().unwrap_or_default().0,
            );
        }
        second.insert(
            "inputs.parameters".to_owned(),
            parameters_json(&after.inputs.parameters),
        );
        for input in &after.inputs.artifacts {
            if !input.path.is_empty() {
                second.insert(
                    format!("inputs.artifacts.{}.path", input.name),
                    input.path.clone(),
                );
            }
        }
        for output in &after.outputs.artifacts {
            if !output.path.is_empty() {
                second.insert(
                    format!("outputs.artifacts.{}.path", output.name),
                    output.path.clone(),
                );
            }
        }
        for output in &after.outputs.parameters {
            if let Some(from) = &output.value_from
                && !from.path.is_empty()
            {
                second.insert(
                    format!("outputs.parameters.{}.path", output.name),
                    from.path.clone(),
                );
            }
        }
        replace_node(&mut node, &Variables::new(second), true).map_err(Failure::error)?;
        let (template, _) = decode::<Template>(&node)
            .map_err(|error| Failure::error(format!("template {}: {error}", typed.name)))?;
        // Paths of input artifacts may hold tags that the passes resolved.
        for (artifact, input) in artifacts.iter_mut().zip(&template.inputs.artifacts) {
            artifact.path = input.path.clone();
        }
        Ok(Processed {
            node,
            template,
            context: Rc::clone(context),
            artifacts,
        })
    }

    /// The artifact that a declared artifact names by its location, if it has one.
    pub(crate) fn artifact_value(&mut self, artifact: &Artifact) -> Result<ArtifactValue> {
        let location = self.location(artifact)?;
        Ok(ArtifactValue {
            name: artifact.name.clone(),
            path: artifact.path.clone(),
            location,
            optional: artifact.optional,
            directory: artifact.git.is_some(),
        })
    }

    fn location(&mut self, artifact: &Artifact) -> Result<Option<Location>> {
        let text = |node: &Node, key: &str| node.get(key).map(scalar_text).unwrap_or_default();
        if let Some(raw) = &artifact.raw {
            return Ok(Some(Location::Raw(text(raw, "data"))));
        }
        let url = if let Some(s3) = &artifact.s3 {
            let key = text(s3, "key");
            let bucket = text(s3, "bucket");
            if key.is_empty() && bucket.is_empty() {
                return Ok(None);
            }
            let bucket = if bucket.is_empty() {
                self.warn("artifacts with only a key use the default artifact repository, here s3://default-artifact-repository".to_owned());
                "default-artifact-repository".to_owned()
            } else {
                bucket
            };
            format!("s3://{bucket}/{}", key.trim_start_matches('/'))
        } else if let Some(http) = &artifact.http {
            text(http, "url")
        } else if let Some(git) = &artifact.git {
            let repository = text(git, "repo");
            match text(git, "revision") {
                revision if revision.is_empty() => repository,
                revision => format!("{repository}#{revision}"),
            }
        } else if let Some(gcs) = &artifact.gcs {
            format!("gs://{}/{}", text(gcs, "bucket"), text(gcs, "key"))
        } else if let Some(azure) = &artifact.azure {
            format!(
                "{}/{}/{}",
                text(azure, "endpoint").trim_end_matches('/'),
                text(azure, "container"),
                text(azure, "blob")
            )
        } else if let Some(artifactory) = &artifact.artifactory {
            text(artifactory, "url")
        } else if let Some(hdfs) = &artifact.hdfs {
            format!("hdfs://{}", text(hdfs, "path").trim_start_matches('/'))
        } else if let Some(oss) = &artifact.oss {
            format!("oss://{}/{}", text(oss, "bucket"), text(oss, "key"))
        } else if artifact.plugin.is_some() {
            return Err(Failure::unsupported(format!(
                "artifact {}: artifact plugins are not supported",
                artifact.name
            )));
        } else {
            return Ok(None);
        };
        // An artifact that a task of this run wrote is read from the task's output.
        if let Some(file) = self.store.get(&url) {
            return Ok(Some(Location::File(file.clone())));
        }
        Ok(Some(Location::Url(url)))
    }
}

fn template_label(template: &Node) -> String {
    template
        .get("name")
        .and_then(Node::as_str)
        .unwrap_or("(inline)")
        .to_owned()
}

fn set_input_value(node: &mut Node, position: usize, value: &str) {
    let Some(inputs) = entry_mut(node, "inputs") else {
        return;
    };
    let Some(Node {
        value: Yaml::Sequence(parameters),
        ..
    }) = entry_mut(inputs, "parameters")
    else {
        return;
    };
    let Some(parameter) = parameters.get_mut(position) else {
        return;
    };
    let text = Node::new(Yaml::String(value.to_owned()), parameter.position);
    match entry_mut(parameter, "value") {
        Some(existing) => *existing = text,
        None => {
            if let Yaml::Mapping(entries) = &mut parameter.value {
                entries.push(Entry {
                    key: "value".into(),
                    position: parameter.position,
                    value: text,
                });
            }
        }
    }
}
