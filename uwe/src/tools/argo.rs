//! Tool definitions of Argo container and script templates, and the workflow that runs one of
//! them on its own as a wir op.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use argo_parser::model::{Artifact, Parameter, Template};
use argo_parser::{Node, Value};
use serde_json::{Map, Value as Json, json};
use tool_definition::{
    Curation, Definition, Image, InputPort, OutputFormat, OutputPort, PortKind, Resources,
    SourceKind, ValueType,
};
use wir::Port;

use super::lowering::name;

/// The kinds of Argo documents whose templates are tools.
const KINDS: [&str; 4] = [
    "Workflow",
    "WorkflowTemplate",
    "ClusterWorkflowTemplate",
    "CronWorkflow",
];

/// Whether a YAML text is an Argo workflow or workflow template, by its API group and kind.
pub(crate) fn is_argo(text: &str) -> bool {
    text.contains("argoproj.io/")
        && (text.lines()).any(|line| {
            let kind = line.trim().strip_prefix("kind:").map(str::trim);
            kind.is_some_and(|kind| KINDS.contains(&kind.trim_matches(['"', '\''])))
        })
}

/// The definitions of the container and script templates of an Argo file, one per template.
pub(crate) fn definitions(path: &Path) -> Result<Vec<Definition>, String> {
    let source = super::source(SourceKind::Argo, path)?;
    let found: Vec<Definition> = (templates(path)?.iter())
        .map(|(template, _)| definition(template, source.clone()))
        .collect();
    match found.is_empty() {
        true => Err("the Argo file has no container or script template".to_owned()),
        false => Ok(found),
    }
}

/// The container and script templates of the Argo documents of a file, each with its node.
fn templates(path: &Path) -> Result<Vec<(Template, Node)>, String> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let documents = argo_parser::parse_documents(&text)
        .map_err(|error| format!("{}: {}", error.position, error.message))?;
    let mut found = Vec::new();
    for document in &documents {
        let api = document.get("apiVersion").and_then(Node::as_str);
        let kind = document.get("kind").and_then(Node::as_str);
        if !api.is_some_and(|api| api.starts_with("argoproj.io/"))
            || !kind.is_some_and(|kind| KINDS.contains(&kind))
        {
            continue;
        }
        let spec = document.get("spec");
        let spec = match kind {
            Some("CronWorkflow") => spec.and_then(|spec| spec.get("workflowSpec")),
            _ => spec,
        };
        let Some(Value::Sequence(nodes)) = spec
            .and_then(|spec| spec.get("templates"))
            .map(|n| &n.value)
        else {
            continue;
        };
        for node in nodes {
            let (template, _) = argo_parser::decode::<Template>(node)
                .map_err(|error| format!("a template: {error}"))?;
            if template.container.is_some() || template.script.is_some() {
                found.push((template, node.clone()));
            }
        }
    }
    Ok(found)
}

fn definition(template: &Template, source: tool_definition::Source) -> Definition {
    let container = template.container.as_ref().or(template.script.as_ref());
    let image = (container.map(|container| container.image.trim()))
        .filter(|image| !image.is_empty())
        .map(|name| Image {
            name: name.to_owned(),
            digest: None,
        });
    let inputs = (template.inputs.artifacts.iter())
        .map(|artifact| InputPort {
            name: artifact.name.clone(),
            title: None,
            description: None,
            kind: PortKind::File,
            cardinality: Default::default(),
            formats: format(artifact).into_iter().collect(),
            unit: None,
            required: !artifact.optional && !located(artifact),
            default: None,
            examples: Vec::new(),
            conditions: Vec::new(),
            active_when: None,
        })
        .collect();
    let mut properties = Map::new();
    let mut required = Vec::new();
    for parameter in &template.inputs.parameters {
        let mut schema = json!({"type": "string"});
        match default(parameter) {
            Some(value) => schema["default"] = json!(value),
            None => required.push(json!(parameter.name)),
        }
        if let Some(text) = &parameter.description {
            schema["description"] = json!(text.0);
        }
        let symbols: Vec<&str> = (parameter.enumeration.iter())
            .map(|symbol| symbol.0.as_str())
            .collect();
        if !symbols.is_empty() {
            schema["enum"] = json!(symbols);
        }
        properties.insert(parameter.name.clone(), schema);
    }
    let mut parameters = Json::Null;
    if !properties.is_empty() {
        parameters = json!({"type": "object", "properties": properties});
        if !required.is_empty() {
            parameters["required"] = Json::Array(required);
        }
    }
    let values = (template.outputs.parameters.iter()).map(|parameter| OutputPort {
        name: parameter.name.clone(),
        title: None,
        description: (parameter.description.as_ref()).map(|text| text.0.clone()),
        kind: PortKind::Value(ValueType::String),
        cardinality: Default::default(),
        format: OutputFormat::Terms(Vec::new()),
        guarantees: Vec::new(),
        created_when: None,
        optional: false,
        schema: json!({"type": "string"}),
    });
    let files = (template.outputs.artifacts.iter()).map(|artifact| OutputPort {
        name: artifact.name.clone(),
        title: None,
        description: None,
        kind: PortKind::File,
        cardinality: Default::default(),
        format: OutputFormat::Terms(format(artifact).into_iter().collect()),
        guarantees: Vec::new(),
        created_when: None,
        optional: artifact.optional,
        schema: Json::Null,
    });
    Definition {
        id: template.name.clone(),
        version: (image.as_ref())
            .and_then(|image| super::tag(&image.name))
            .unwrap_or_else(|| "1.0".to_owned()),
        title: template.name.clone(),
        description: None,
        source: Some(source),
        image,
        license: None,
        authors: Vec::new(),
        citations: Vec::new(),
        biotools: None,
        operations: Vec::new(),
        topics: Vec::new(),
        keywords: Vec::new(),
        inputs,
        parameters,
        outputs: values.chain(files).collect(),
        resources: container
            .and_then(|container| container.resources.as_ref())
            .map(resources)
            .unwrap_or_default(),
        tests: Vec::new(),
        curation: Curation::Unchecked,
    }
}

/// The default of an input parameter: its `default`, else the `value` it is fixed to.
fn default(parameter: &Parameter) -> Option<&str> {
    (parameter.default.as_ref())
        .or(parameter.value.as_ref())
        .map(|value| value.0.as_str())
}

/// Whether an input artifact names where its data comes from, so that nothing must feed it.
fn located(artifact: &Artifact) -> bool {
    [
        &artifact.raw,
        &artifact.s3,
        &artifact.gcs,
        &artifact.azure,
        &artifact.git,
        &artifact.http,
        &artifact.artifactory,
    ]
    .into_iter()
    .any(Option::is_some)
}

/// The EDAM format of the ending of an artifact's path, else of its name.
fn format(artifact: &Artifact) -> Option<String> {
    (tool_definition::formats::ending(&artifact.path))
        .or_else(|| tool_definition::formats::ending(&artifact.name))
        .map(str::to_owned)
}

/// The CPUs and memory that a container asks for, from its requests, else its limits.
fn resources(node: &Node) -> Resources {
    let amount = |key: &str| {
        ["requests", "limits"]
            .iter()
            .find_map(|scope| node.get(scope)?.get(key))
            .map(|value| match &value.value {
                Value::String(text) => text.trim().to_owned(),
                Value::Integer(number) => number.to_string(),
                Value::Unsigned(number) => number.to_string(),
                Value::Float(number) => number.to_string(),
                _ => String::new(),
            })
    };
    Resources {
        cpus: amount("cpu").and_then(|text| cpus(&text)),
        memory: amount("memory").and_then(|text| memory(&text)),
        ..Resources::default()
    }
}

/// Whole CPUs of a Kubernetes quantity, like 2 for `2` or 1 for `500m`.
fn cpus(text: &str) -> Option<u32> {
    let cores = match text.strip_suffix('m') {
        Some(milli) => milli.parse::<f64>().ok()? / 1000.0,
        None => text.parse::<f64>().ok()?,
    };
    (cores > 0.0).then(|| cores.ceil() as u32)
}

/// The bytes of a Kubernetes quantity, like `512Mi`, `1G` or `1048576`.
fn memory(text: &str) -> Option<u64> {
    let split = text
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: f64 = number.parse().ok()?;
    let factor: f64 = match unit {
        "" => 1.0,
        "k" => 1e3,
        "M" => 1e6,
        "G" => 1e9,
        "T" => 1e12,
        "Ki" => 1024.0,
        "Mi" => 1024f64.powi(2),
        "Gi" => 1024f64.powi(3),
        "Ti" => 1024f64.powi(4),
        _ => return None,
    };
    Some((number * factor) as u64)
}

/// The Argo workflow that runs the template of `definition` from `file` on its own, and the
/// name of its entry template. Its entry takes the op inputs as parameters, hands the paths of
/// file inputs to the template as artifacts, and gives the template's outputs as its own.
pub(crate) fn entry(
    file: &Path,
    definition: &Definition,
    ports: &[Port],
) -> Result<(String, String), String> {
    let found = templates(file)?;
    let (template, node) = (found.iter())
        .find(|(template, _)| template.name == definition.id)
        .ok_or_else(|| format!("{} has no template {}", file.display(), definition.id))?;
    let given = |part: &str| ports.iter().any(|port| port.name == name(part));
    let step = template.name.clone();
    let mut outer = "node".to_owned();
    while outer == step {
        outer.push_str("-entry");
    }
    let mut inputs = Vec::new();
    let mut parameters = Vec::new();
    for parameter in &template.inputs.parameters {
        // A parameter with a fixed value takes no argument, as in Argo.
        if parameter.value.is_some() && parameter.default.is_none() {
            continue;
        }
        let mut input = json!({"name": parameter.name});
        if let Some(value) = parameter
            .default
            .as_ref()
            .filter(|_| !given(&parameter.name))
        {
            input["default"] = json!(value.0);
        }
        inputs.push(input);
        let value = format!("{{{{inputs.parameters.{}}}}}", parameter.name);
        parameters.push(json!({"name": parameter.name, "value": value}));
    }
    let mut artifacts = Vec::new();
    for artifact in &template.inputs.artifacts {
        if !given(&artifact.name) && (artifact.optional || located(artifact)) {
            continue;
        }
        inputs.push(json!({"name": artifact.name}));
        let url = format!("{{{{inputs.parameters.{}}}}}", artifact.name);
        artifacts.push(json!({"name": artifact.name, "http": {"url": url}}));
    }
    let mut arguments = Map::new();
    if !parameters.is_empty() {
        arguments.insert("parameters".into(), Json::Array(parameters));
    }
    if !artifacts.is_empty() {
        arguments.insert("artifacts".into(), Json::Array(artifacts));
    }
    let mut call = json!({"name": step, "template": template.name});
    if !arguments.is_empty() {
        call["arguments"] = Json::Object(arguments);
    }
    let values: Vec<Json> = (template.outputs.parameters.iter())
        .map(|parameter| {
            let from = format!("{{{{steps.{step}.outputs.parameters.{}}}}}", parameter.name);
            json!({"name": parameter.name, "valueFrom": {"parameter": from}})
        })
        .collect();
    let files: Vec<Json> = (template.outputs.artifacts.iter())
        .map(|artifact| {
            let from = format!("{{{{steps.{step}.outputs.artifacts.{}}}}}", artifact.name);
            let mut output = json!({"name": artifact.name, "from": from});
            if artifact.optional {
                output["optional"] = json!(true);
            }
            output
        })
        .collect();
    let mut outputs = Map::new();
    if !values.is_empty() {
        outputs.insert("parameters".into(), Json::Array(values));
    }
    if !files.is_empty() {
        outputs.insert("artifacts".into(), Json::Array(files));
    }
    let mut wrapper = json!({"name": outer, "steps": [[call]]});
    if !inputs.is_empty() {
        wrapper["inputs"] = json!({"parameters": inputs});
    }
    if !outputs.is_empty() {
        wrapper["outputs"] = Json::Object(outputs);
    }
    let workflow = json!({
        "apiVersion": "argoproj.io/v1alpha1",
        "kind": "Workflow",
        "metadata": {"generateName": format!("{}-", name(&template.name).to_lowercase())},
        "spec": {"entrypoint": outer, "templates": [wrapper, value(node)]},
    });
    let text = format!(
        "# The template {} as one wir op, made by `uwe lower`.\n{}",
        template.name,
        tool_definition::yaml::write(&workflow)
    );
    Ok((text, outer))
}

/// A node of an Argo document as JSON.
fn value(node: &Node) -> Json {
    match &node.value {
        Value::Null => Json::Null,
        Value::Bool(flag) => Json::Bool(*flag),
        Value::Integer(number) => json!(number),
        Value::Unsigned(number) => json!(number),
        Value::Float(number) => json!(number),
        Value::String(text) => Json::String(text.clone()),
        Value::Sequence(items) => Json::Array(items.iter().map(value).collect()),
        Value::Mapping(entries) => Json::Object(
            (entries.iter())
                .map(|entry| (entry.key.clone(), value(&entry.value)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATE: &str = r#"apiVersion: argoproj.io/v1alpha1
kind: WorkflowTemplate
metadata:
  name: texts
spec:
  templates:
    - name: main
      steps:
        - - name: count
            template: count
    - name: count
      inputs:
        parameters:
          - name: prefix
            default: "n="
          - name: mode
        artifacts:
          - name: text
            path: /in/text.txt
      container:
        image: debian:stable-slim
        command: [sh, -c]
        args: ["echo {{inputs.parameters.prefix}} > /out/count.txt"]
        resources:
          requests:
            cpu: 500m
            memory: 256Mi
      outputs:
        parameters:
          - name: lines
            valueFrom:
              path: /out/lines
        artifacts:
          - name: counted
            path: /out/count.txt
"#;

    #[test]
    fn reads_templates() {
        assert!(is_argo(TEMPLATE));
        assert!(!is_argo("kind: Workflow\n"), "no Argo API group");
        let folder = tempfile::tempdir().expect("a folder");
        let file = folder.path().join("texts.yaml");
        std::fs::write(&file, TEMPLATE).expect("writes the template");
        let found = definitions(&file).expect("definitions");
        assert_eq!(found.len(), 1, "only the container template is a tool");
        let count = &found[0];
        assert_eq!(
            (count.id.as_str(), count.version.as_str()),
            ("count", "stable-slim")
        );
        assert_eq!(count.inputs[0].formats, ["format_2330"]);
        assert_eq!(count.parameters["properties"]["prefix"]["default"], "n=");
        assert_eq!(count.parameters["required"], json!(["mode"]));
        let outputs: Vec<(&str, PortKind)> = (count.outputs.iter())
            .map(|port| (port.name.as_str(), port.kind))
            .collect();
        assert_eq!(
            outputs,
            [
                ("lines", PortKind::Value(ValueType::String)),
                ("counted", PortKind::File)
            ]
        );
        assert_eq!(
            (count.resources.cpus, count.resources.memory),
            (Some(1), Some(256 << 20))
        );
    }

    #[test]
    fn wraps_one_template() {
        let folder = tempfile::tempdir().expect("a folder");
        let file = folder.path().join("texts.yaml");
        std::fs::write(&file, TEMPLATE).expect("writes the template");
        let found = definitions(&file).expect("definitions");
        let port = |name: &str| Port {
            name: name.to_owned(),
            r#type: wir::Type::String,
            flow: Default::default(),
            default: None,
        };
        let (text, outer) =
            entry(&file, &found[0], &[port("text"), port("mode")]).expect("a workflow");
        assert_eq!(outer, "node");
        let documents = argo_runtime::load_file(&{
            let written = folder.path().join("entry.yaml");
            std::fs::write(&written, &text).expect("writes the workflow");
            written
        })
        .expect("the workflow loads");
        assert_eq!(documents.len(), 1, "{text}");
        assert!(
            text.contains("url: \"{{inputs.parameters.text}}\""),
            "a file input reaches the template as an artifact: {text}"
        );
        assert!(
            text.contains("from: \"{{steps.count.outputs.artifacts.counted}}\""),
            "{text}"
        );
        assert!(
            text.contains("default: n="),
            "an input nothing feeds keeps its default: {text}"
        );
    }
}
