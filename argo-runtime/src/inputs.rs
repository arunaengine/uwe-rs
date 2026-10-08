//! Arguments into inputs, as Argo's `ProcessArgs` and `SubstituteParams` do: the values of input
//! parameters, the substitution of a template in two phases, and where artifacts come from.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use argo_parser::model::{AnyString, Artifact, Template};
use argo_parser::{Entry, Node, Value as Yaml, decode};

use crate::node::{ArtifactValue, Location};
use crate::objects::{KeyRef, config_map_value};
use crate::run::{
    Args, Context, Failure, Processed, Result, Run, entry_mut, is_pod, parameters_json, scalar_text,
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
        if let Some(reason) = crate::checks::cluster_only(&typed) {
            return Err(Failure::unsupported(reason));
        }
        let mut node = template.clone();
        let mut values: Vec<(String, String)> = Vec::new();
        for (position, input) in typed.inputs.parameters.iter().enumerate() {
            let mut value = input.value.clone().or_else(|| input.default.clone());
            let mut from = input.value_from.clone();
            if let Some(argument) = args
                .parameters
                .iter()
                .find(|parameter| parameter.name == input.name)
            {
                if let Some(given) = &argument.value {
                    value = Some(given.clone());
                    from = None;
                } else if let Some(given) = &argument.value_from {
                    if !given.event.is_empty()
                        || given.supplied.is_some()
                        || !given.jq_filter.is_empty()
                    {
                        return Err(Failure::unsupported(format!(
                            "input {}: valueFrom event, supplied and jqFilter sources are not supported",
                            input.name
                        )));
                    }
                    if given.config_key_ref.is_some() {
                        from = Some(given.clone());
                    } else if !given.supplied.is_none() || !given.expression.is_empty() {
                        value = given.default.clone().or(value);
                    }
                }
            }
            // A ConfigMap wins; without it the default of `valueFrom` takes over, as in Argo.
            if let Some(from) = from.filter(|from| from.config_key_ref.is_some()) {
                let variables = Variables::new(self.globals.clone());
                let mut reference = from
                    .config_key_ref
                    .as_ref()
                    .map(KeyRef::of)
                    .unwrap_or_default();
                for text in [&mut reference.name, &mut reference.key] {
                    *text = crate::tags::replace(text, &variables, true).map_err(Failure::error)?;
                }
                let what = format!("input {} of template {}", input.name, typed.name);
                value = Some(AnyString(
                    match config_map_value(&self.library, &self.document.path, &reference) {
                        Some(found) => found,
                        None => match from.default {
                            Some(default) => default.0,
                            None => self.config_map_key(&what, &reference)?,
                        },
                    },
                ));
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
        let mut deferred_sub_paths = Vec::new();
        for input in &typed.inputs.artifacts {
            let argument = args
                .artifacts
                .iter()
                .find(|artifact| artifact.name == input.name);
            let from_argument = argument.is_some_and(|argument| argument.location.is_some());
            let mut value = match argument.filter(|argument| argument.location.is_some()) {
                Some(argument) => argument.clone(),
                None => self.artifact_value(input)?,
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
            deferred_sub_paths.push(from_argument || crate::tags::has_tag(&input.sub_path));
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
        for (index, (artifact, input)) in artifacts
            .iter_mut()
            .zip(&template.inputs.artifacts)
            .enumerate()
        {
            artifact.path = input.path.clone();
            if input
                .archive
                .as_ref()
                .is_some_and(|node| node.get("none").is_none())
                && !matches!(artifact.location, Some(Location::File(_)))
            {
                return Err(Failure::unsupported(format!(
                    "artifact {}: archive extraction requires an output produced by this run; external archive inputs are not supported",
                    input.name
                )));
            }
            if deferred_sub_paths[index] {
                apply_sub_path(artifact, &input.sub_path)?;
            }
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
        let mut value = ArtifactValue {
            name: artifact.name.clone(),
            path: artifact.path.clone(),
            location,
            optional: artifact.optional,
            directory: artifact.git.is_some(),
        };
        if !crate::tags::has_tag(&artifact.sub_path) {
            apply_sub_path(&mut value, &artifact.sub_path)?;
        }
        Ok(value)
    }

    fn location(&mut self, artifact: &Artifact) -> Result<Option<Location>> {
        check_artifact(artifact)?;
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
                return Err(Failure::unsupported(format!(
                    "artifact {}: the default artifact repository is not supported; supply an S3 bucket",
                    artifact.name
                )));
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

pub(crate) fn check_artifact(artifact: &Artifact) -> Result<()> {
    if artifact.git.is_some()
        || artifact.hdfs.is_some()
        || artifact.oss.is_some()
        || artifact.plugin.is_some()
    {
        return Err(Failure::unsupported(format!(
            "artifact {}: Git, HDFS, OSS and artifact plugin transfers are not supported",
            artifact.name
        )));
    }
    if artifact.mode.is_some()
        || artifact.recurse_mode.is_some()
        || artifact.archive.as_ref().is_some_and(|node| {
            node.get("none").is_none()
                && (artifact.raw.is_some()
                    || artifact.s3.is_some()
                    || artifact.http.is_some()
                    || artifact.gcs.is_some()
                    || artifact.azure.is_some()
                    || artifact.artifactory.is_some())
        })
        || artifact
            .archive_logs
            .as_ref()
            .is_some_and(|node| node.value != Yaml::Bool(false))
    {
        return Err(Failure::unsupported(format!(
            "artifact {}: mode, recurseMode, archive compression and archiveLogs are not supported",
            artifact.name
        )));
    }
    let locations: [(_, _, &[&str]); 6] = [
        ("S3", artifact.s3.as_ref(), &["bucket", "key"]),
        ("HTTP", artifact.http.as_ref(), &["url"]),
        ("GCS", artifact.gcs.as_ref(), &["bucket", "key"]),
        (
            "Azure",
            artifact.azure.as_ref(),
            &["endpoint", "container", "blob"],
        ),
        ("Artifactory", artifact.artifactory.as_ref(), &["url"]),
        ("raw", artifact.raw.as_ref(), &["data"]),
    ];
    for (kind, node, supported) in locations {
        if let Some(Node {
            value: Yaml::Mapping(fields),
            ..
        }) = node
        {
            for field in fields {
                if !supported.contains(&field.key.as_str()) && field.value.value != Yaml::Null {
                    if kind == "S3" && field.key == "endpoint" {
                        return Err(Failure::unsupported(format!(
                            "artifact {}: S3.endpoint cannot be verified against the configured store because its endpoint identity is not available to the Argo driver",
                            artifact.name
                        )));
                    }
                    return Err(Failure::unsupported(format!(
                        "artifact {}: {kind}.{} is not supported; use the configured storage provider",
                        artifact.name, field.key
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Selects a child of a materialized directory artifact without leaving that directory.
fn apply_sub_path(artifact: &mut ArtifactValue, sub_path: &str) -> Result<()> {
    if sub_path.is_empty() {
        return Ok(());
    }
    if crate::tags::has_tag(sub_path) {
        return Err(Failure::error(format!(
            "artifact {} has unresolved subPath {sub_path}",
            artifact.name
        )));
    }
    let mut relative = PathBuf::new();
    for component in Path::new(sub_path).components() {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            _ => {
                return Err(Failure::error(format!(
                    "artifact {} has invalid subPath {sub_path}",
                    artifact.name
                )));
            }
        }
    }
    let Some(Location::File(root)) = &artifact.location else {
        let kind = match &artifact.location {
            Some(Location::Raw(_)) => "raw content",
            Some(Location::Url(_)) => "an unmaterialized remote source",
            _ => "no source",
        };
        return Err(Failure::unsupported(format!(
            "artifact {} cannot select subPath {sub_path} from {kind}",
            artifact.name
        )));
    };
    if !root.is_dir() {
        return Err(Failure::error(format!(
            "artifact {} cannot select subPath {sub_path} from the file {}",
            artifact.name,
            root.display()
        )));
    }
    let selected = root.join(relative);
    let base = root.canonicalize().map_err(|error| {
        Failure::error(format!(
            "cannot inspect artifact {}: {error}",
            artifact.name
        ))
    })?;
    let canonical = selected.canonicalize().map_err(|error| {
        Failure::error(format!(
            "artifact {} subPath {sub_path} cannot be selected: {error}",
            artifact.name
        ))
    })?;
    if !canonical.starts_with(&base) {
        return Err(Failure::error(format!(
            "artifact {} subPath {sub_path} escapes {}",
            artifact.name,
            root.display()
        )));
    }
    artifact.directory = selected.is_dir();
    artifact.location = Some(Location::File(selected));
    Ok(())
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
