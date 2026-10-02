//! Describes Argo workflow parameters and artifacts as JSON Schema.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;
use std::path::PathBuf;

use argo_parser::model::{Artifact, Parameter, Template, WorkflowSpec};
use argo_parser::{Node, Value as Yaml, decode};
use input_schema::{DRAFT, PATH};
use serde_json::{Map, Value as Json, json};
use workflow_graph::Inputs;

use crate::Options;
use crate::library::{Document, Kind, Library};
use crate::merge::with_defaults;
use crate::run::{Failure, Result, scalar_text};
use crate::start::{Joined, joined_spec, workflow_parameters};

/// The annotation that holds the description of a workflow in Argo's user interface.
const DESCRIPTION: &str = "workflows.argoproj.io/description";

/// The inputs of the workflow that a document runs.
pub(crate) fn inputs(document: &Document, library: &Library, options: &Options) -> Result<Inputs> {
    let Joined { spec: node, .. } = joined_spec(document, library)?;
    let (spec, _) =
        decode::<WorkflowSpec>(&node).map_err(|error| Failure::error(format!("spec: {error}")))?;
    let entrypoint = (options.entrypoint.clone())
        .filter(|entrypoint| !entrypoint.is_empty())
        .unwrap_or_else(|| spec.entrypoint.clone());
    let entry_node = template(&node, &entrypoint).map(|entry| match node.get("templateDefaults") {
        Some(defaults) => with_defaults(entry, defaults),
        None => entry.clone(),
    });
    let entry_node = entry_node.as_ref();
    let entry = entry_node
        .and_then(|entry| decode::<Template>(entry).ok())
        .map(|(template, _)| template)
        .unwrap_or_default();
    // The run warns about what it makes up itself.
    let mut warnings = Vec::new();
    let given = workflow_parameters(
        &spec,
        entry_node,
        options,
        (library, &document.path),
        &mut warnings,
    )?;
    let mut properties = Map::new();
    let mut required = Vec::new();
    let supplied = |name: &str| spec.arguments.parameters.iter().any(|p| p.name == name);
    let entry_inputs = entry
        .inputs
        .parameters
        .iter()
        .filter(|p| !supplied(&p.name));
    for parameter in spec.arguments.parameters.iter().chain(entry_inputs) {
        let schema = parameter_schema(parameter);
        if schema.get("default").is_none() && parameter.value_from.is_none() {
            required.push(json!(parameter.name));
        }
        properties.insert(parameter.name.clone(), schema);
    }
    let argument = |name: &str| spec.arguments.artifacts.iter().any(|a| a.name == name);
    let entry_artifacts = entry.inputs.artifacts.iter().filter(|a| !argument(&a.name));
    let mut values = Map::new();
    for artifact in spec.arguments.artifacts.iter().chain(entry_artifacts) {
        let mut schema = json!({"type": "string", "format": PATH, "readOnly": true});
        if let Some(location) = location(artifact)? {
            schema["default"] = json!(location);
            values.insert(artifact.name.clone(), json!(location));
        } else if !artifact.optional {
            required.push(json!(artifact.name));
        }
        properties.insert(artifact.name.clone(), schema);
    }
    for parameter in &given.parameters {
        if let Some(value) = &parameter.value {
            values.insert(parameter.name.clone(), json!(value.0));
        }
    }
    let mut schema = json!({
        "$schema": DRAFT,
        "title": title(document),
        "type": "object",
        "properties": properties,
    });
    if let Some(description) = annotation(document, DESCRIPTION) {
        schema["description"] = json!(description);
    }
    if !required.is_empty() {
        schema["required"] = Json::Array(required);
    }
    let read = read(&node, library, &document.path, entry_node, &entry, &spec);
    Ok(Inputs {
        schema,
        values: Json::Object(values),
        base: std::env::current_dir().unwrap_or_default(),
        read: Some(read),
        assumes_values: options.assume_inputs,
        advisory: false,
        ignored: Default::default(),
    })
}

/// The outputs of the workflow that a document runs: the output parameters of its entrypoint,
/// which are texts, and its output artifacts, which are paths, then the global outputs of its
/// templates by their `globalName`, as `workflow.outputs` gives them. An artifact that is
/// `optional` may be missing.
pub(crate) fn outputs(document: &Document, library: &Library, options: &Options) -> Result<Json> {
    let Joined { spec: node, .. } = joined_spec(document, library)?;
    let (spec, _) =
        decode::<WorkflowSpec>(&node).map_err(|error| Failure::error(format!("spec: {error}")))?;
    let entrypoint = (options.entrypoint.clone())
        .filter(|entrypoint| !entrypoint.is_empty())
        .unwrap_or_else(|| spec.entrypoint.clone());
    let entry = spec
        .templates
        .iter()
        .find(|template| template.name == entrypoint);
    let mut properties = Map::new();
    let mut required = Vec::new();
    let mut add = |name: &str, schema: Json, optional: bool| {
        if properties.contains_key(name) {
            return;
        }
        if !optional {
            required.push(json!(name));
        }
        properties.insert(name.to_owned(), schema);
    };
    if let Some(entry) = entry {
        for parameter in &entry.outputs.parameters {
            add(&parameter.name, output_parameter(parameter, None), false);
        }
        for artifact in &entry.outputs.artifacts {
            add(&artifact.name, output_artifact(None), artifact.optional);
        }
    }
    for template in &spec.templates {
        for parameter in &template.outputs.parameters {
            if !parameter.global_name.is_empty() {
                let schema = output_parameter(parameter, Some(&template.name));
                add(&parameter.global_name, schema, true);
            }
        }
        for artifact in &template.outputs.artifacts {
            if !artifact.global_name.is_empty() {
                let schema = output_artifact(Some(&template.name));
                add(&artifact.global_name, schema, true);
            }
        }
    }
    let mut schema = json!({
        "$schema": DRAFT,
        "title": title(document),
        "type": "object",
        "properties": properties,
    });
    if !required.is_empty() {
        schema["required"] = Json::Array(required);
    }
    Ok(schema)
}

/// The schema of an output parameter: a text, with its description, and the template that
/// gives it when it is a global output.
fn output_parameter(parameter: &Parameter, template: Option<&str>) -> Json {
    let mut schema = json!({"type": "string"});
    let description = parameter
        .description
        .as_ref()
        .map(|d| d.as_str().to_owned());
    let description = match (description, template) {
        (Some(description), _) => Some(description),
        (None, Some(template)) => Some(format!("a global output of the template {template}")),
        (None, None) => None,
    };
    if let Some(description) = description {
        schema["description"] = json!(description);
    }
    schema
}

/// The schema of an output artifact: a file or directory that the run makes.
fn output_artifact(template: Option<&str>) -> Json {
    let mut schema = json!({"type": "string", "format": PATH});
    if let Some(template) = template {
        schema["description"] = json!(format!("a global output of the template {template}"));
    }
    schema
}

/// The schema of a parameter: a text, with its choices, its value or default as the default,
/// and its description.
fn parameter_schema(parameter: &Parameter) -> Json {
    let mut schema = json!({"type": "string"});
    if !parameter.enumeration.is_empty() {
        let choices: Vec<&str> = parameter.enumeration.iter().map(|c| c.as_str()).collect();
        schema["enum"] = json!(choices);
    }
    let default = (parameter.value.as_ref())
        .or(parameter.default.as_ref())
        .or_else(|| parameter.value_from.as_ref()?.default.as_ref());
    if let Some(default) = default {
        schema["default"] = json!(default.as_str());
    }
    if let Some(description) = &parameter.description {
        schema["description"] = json!(description.as_str());
    }
    schema
}

/// Where an artifact comes from, as an address, when it names one.
fn location(artifact: &Artifact) -> Result<Option<String>> {
    crate::inputs::check_artifact(artifact)?;
    let text = |node: &Node, key: &str| node.get(key).map(scalar_text).unwrap_or_default();
    if let Some(raw) = &artifact.raw {
        return Ok(Some(data_address(&text(raw, "data"))));
    }
    let location = if let Some(s3) = &artifact.s3 {
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
            &bucket
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
    } else if artifact.hdfs.is_some() || artifact.oss.is_some() || artifact.plugin.is_some() {
        let kind = if artifact.hdfs.is_some() {
            "HDFS"
        } else if artifact.oss.is_some() {
            "OSS"
        } else {
            "plugin"
        };
        return Err(Failure::unsupported(format!(
            "input artifact {} uses {kind}, which is not supported",
            artifact.name
        )));
    } else {
        return Ok(None);
    };
    Ok(Some(location))
}

fn data_address(text: &str) -> String {
    let mut address = String::from("data:,");
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            address.push(char::from(byte));
        } else {
            address.push_str(&format!("%{byte:02X}"));
        }
    }
    address
}

/// The template of a spec by its name.
fn template<'a>(spec: &'a Node, name: &str) -> Option<&'a Node> {
    match &spec.get("templates")?.value {
        Yaml::Sequence(templates) => templates
            .iter()
            .find(|template| template.get("name").and_then(Node::as_str) == Some(name)),
        _ => None,
    }
}

/// The name of the workflow: its `metadata.name` or `generateName`, else its label.
fn title(document: &Document) -> String {
    let metadata = document.node.get("metadata");
    ["name", "generateName"]
        .iter()
        .find_map(|key| metadata?.get(key).and_then(Node::as_str))
        .filter(|name| !name.is_empty())
        .map_or_else(|| document.label(), str::to_owned)
}

fn annotation(document: &Document, key: &str) -> Option<String> {
    let annotations = document.node.get("metadata")?.get("annotations")?;
    annotations
        .get(key)
        .and_then(Node::as_str)
        .map(str::to_owned)
}

/// The parameters that the workflow reads: `workflow.parameters.<name>` anywhere in its spec
/// and in the templates it refers to with `templateRef`, and `inputs.parameters.<name>` in its
/// entrypoint, whose inputs the arguments supply. An input artifact of the entrypoint is read,
/// as Argo places it in the container. `{{workflow.parameters}}` reads every parameter.
fn read(
    spec: &Node,
    library: &Library,
    near: &std::path::Path,
    entry: Option<&Node>,
    template: &Template,
    declared: &WorkflowSpec,
) -> BTreeSet<String> {
    let mut texts = Vec::new();
    let mut seen: BTreeSet<(bool, String)> = BTreeSet::new();
    let mut pending: Vec<(Node, PathBuf)> = vec![(spec.clone(), near.to_owned())];
    while let Some((node, near)) = pending.pop() {
        let mut references = Vec::new();
        strings(&node, &mut texts, &mut references);
        for (cluster, name) in references {
            if !seen.insert((cluster, name.clone())) {
                continue;
            }
            let kind = match cluster {
                true => Kind::ClusterWorkflowTemplate,
                false => Kind::WorkflowTemplate,
            };
            if let Some(found) = library.find(kind, &name, &near)
                && let Some(spec) = found.spec()
            {
                pending.push((spec.clone(), found.path.clone()));
            }
        }
    }
    let mut names = BTreeSet::new();
    for text in &texts {
        names.extend(names_after(text, "workflow.parameters"));
        if text.contains("{{workflow.parameters}}") || text.contains("workflow.parameters.json") {
            names.extend(declared.arguments.parameters.iter().map(|p| p.name.clone()));
        }
    }
    names.remove("json");
    if let Some(entry) = entry {
        let mut own = Vec::new();
        strings(entry, &mut own, &mut Vec::new());
        for text in &own {
            names.extend(names_after(text, "inputs.parameters"));
        }
    }
    names.extend(template.inputs.artifacts.iter().map(|a| a.name.clone()));
    names
}

/// Every text of a node, keys aside, and the templates that its `templateRef`s name, with
/// whether each is cluster-scoped.
fn strings(node: &Node, texts: &mut Vec<String>, references: &mut Vec<(bool, String)>) {
    match &node.value {
        Yaml::String(text) => texts.push(text.clone()),
        Yaml::Sequence(items) => items
            .iter()
            .for_each(|item| strings(item, texts, references)),
        Yaml::Mapping(entries) => {
            for entry in entries {
                if entry.key == "templateRef"
                    && let Some(name) = entry.value.get("name").and_then(Node::as_str)
                {
                    let cluster = entry
                        .value
                        .get("clusterScope")
                        .is_some_and(|scope| scope.value == Yaml::Bool(true));
                    references.push((cluster, name.to_owned()));
                }
                strings(&entry.value, texts, references);
            }
        }
        _ => {}
    }
}

/// The parameter names after `prefix.` or inside `prefix['...']` and `prefix["..."]` in a text.
fn names_after(text: &str, prefix: &str) -> Vec<String> {
    let name_char = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    let mut names = Vec::new();
    for (start, _) in text.match_indices(prefix) {
        let rest = &text[start + prefix.len()..];
        let name: String = if let Some(rest) = rest.strip_prefix('.') {
            rest.chars().take_while(|c| name_char(*c)).collect()
        } else if let Some(rest) = rest.strip_prefix('[') {
            let Some(quote) = rest.chars().next().filter(|c| matches!(c, '\'' | '"')) else {
                continue;
            };
            rest[1..].chars().take_while(|c| *c != quote).collect()
        } else {
            continue;
        };
        if !name.is_empty() {
            names.push(name);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_parameter_names() {
        let text = "echo {{workflow.parameters.message}} {{=workflow.parameters['max-count']}} \
                    {{= sprig.upper(workflow.parameters[\"name\"]) }} {{workflow.parameters}}";
        assert_eq!(
            names_after(text, "workflow.parameters"),
            ["message", "max-count", "name"]
        );
    }
}
