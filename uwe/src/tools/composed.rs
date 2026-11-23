//! Composed workflows: a graph of tool nodes with workflow inputs and outputs, and their
//! human-readable form, the `.uwe.yaml` file, which reads and writes both ways.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Map, Value as Json, json};
use tool_definition::{
    Cardinality, Definition, Edge, End, Evidence, Fact, Graph, Node, PortKind, Shape, Supply,
    ValueType,
};

use super::library::ToolLibrary;

/// The version of the `.uwe.yaml` format that this reader writes and reads.
pub const VERSION: &str = "1";

/// A workflow composed of tools: its nodes and edges, the inputs that feed them, the outputs
/// it gives, and the tools it names.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ComposedWorkflow {
    pub name: String,
    pub description: Option<String>,
    pub inputs: Vec<ComposedInput>,
    /// The nodes and edges; workflow inputs feed the nodes, so `given` stays empty here.
    pub graph: Graph,
    /// The outputs of the workflow; without any, the outputs that no edge reads.
    pub outputs: Vec<ComposedOutput>,
    /// Tool definitions written in the workflow itself.
    pub tools: Vec<Definition>,
    /// Tool files that the workflow names, from its folder: definition files and the tools
    /// of every language uwe reads.
    pub references: Vec<String>,
}

/// An input of the workflow: what it carries and the node inputs it feeds.
#[derive(Clone, Debug, PartialEq)]
pub struct ComposedInput {
    pub name: String,
    pub description: Option<String>,
    pub supply: Supply,
    pub required: bool,
    pub default: Option<Json>,
    /// The inputs or parameters of nodes it feeds.
    pub to: Vec<End>,
}

/// An output of the workflow and the node output it gives.
#[derive(Clone, Debug, PartialEq)]
pub struct ComposedOutput {
    pub name: String,
    pub description: Option<String>,
    pub from: End,
}

/// Whether a file is a composed workflow, by its name, `<name>.uwe.yaml` or `<name>.uwe.yml`.
pub fn is_composed(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase());
    name.is_some_and(|name| name.ends_with(".uwe.yaml") || name.ends_with(".uwe.yml"))
}

/// Reads a composed workflow file, adds the tools it names and holds to `library`, and gives
/// each node without a version the last version of its tool in the library.
pub fn load(path: &Path, library: &mut ToolLibrary) -> Result<ComposedWorkflow, String> {
    let shown = path.display();
    let text = std::fs::read_to_string(path).map_err(|error| format!("{shown}: {error}"))?;
    let mut workflow = parse(&text).map_err(|error| format!("{shown}: {error}"))?;
    let folder = path.parent().unwrap_or(Path::new("."));
    for reference in &workflow.references {
        let file = folder.join(reference);
        let added = match file.is_dir() {
            true => Ok(library.scan(&file)),
            false => library.add_file(&file),
        };
        added.map_err(|error| format!("{shown}: tool {reference}: {error}"))?;
    }
    (workflow.resolve(library, Some(path))).map_err(|error| format!("{shown}: {error}"))?;
    Ok(workflow)
}

impl ComposedWorkflow {
    /// Adds the tools that the workflow holds to `library`, as from the file `origin`, gives
    /// each node without a version the last version of its tool there, and counts the uses.
    pub fn resolve(
        &mut self,
        library: &mut ToolLibrary,
        origin: Option<&Path>,
    ) -> Result<(), String> {
        for tool in &self.tools {
            library.add(tool.clone(), origin.map(Path::to_owned));
        }
        for node in &mut self.graph.nodes {
            if node.version.is_empty() {
                let entry = library.get(&node.tool, None).ok_or_else(|| {
                    format!("step {}: the library has no tool {}", node.id, node.tool)
                })?;
                node.version.clone_from(&entry.definition.version);
            }
            library.count_use(&node.tool);
        }
        Ok(())
    }

    /// The definitions of the library that the nodes use, one per tool version, so that a
    /// check does not copy the whole library.
    pub fn used_tools(&self, library: &ToolLibrary) -> Vec<Definition> {
        let mut used: Vec<Definition> = Vec::new();
        for node in &self.graph.nodes {
            let known =
                (used.iter()).any(|tool| tool.id == node.tool && tool.version == node.version);
            if let (false, Some(entry)) = (known, library.get(&node.tool, Some(&node.version))) {
                used.push(entry.definition.clone());
            }
        }
        used
    }

    /// The graph that `tool_definition::graph::check` checks: each node gets the supplies of
    /// the workflow inputs that feed its ports, and a value for the parameters they feed.
    pub fn graph_with(&self, tools: &[Definition]) -> Graph {
        let mut graph = self.graph.clone();
        for node in &mut graph.nodes {
            let tool =
                (tools.iter()).find(|tool| tool.id == node.tool && tool.version == node.version);
            for input in &self.inputs {
                for end in input.to.iter().filter(|end| end.node == node.id) {
                    let port = tool.and_then(|tool| tool.input(&end.port));
                    if port.is_some() || tool.is_none() {
                        node.given.insert(end.port.clone(), input.supply.clone());
                        continue;
                    }
                    // A parameter that an input feeds gets a value of its kind for the check.
                    let value = input
                        .default
                        .clone()
                        .unwrap_or_else(|| placeholder(input.supply.kind));
                    if !node.parameters.is_object() {
                        node.parameters = json!({});
                    }
                    if let Json::Object(values) = &mut node.parameters {
                        values.entry(end.port.clone()).or_insert(value);
                    }
                }
            }
        }
        graph
    }

    /// The outputs of the workflow: those it names, else each node output that no edge reads,
    /// as `<node>.<output>`.
    pub fn outputs_or_terminal(&self, tools: &[Definition]) -> Vec<ComposedOutput> {
        if !self.outputs.is_empty() {
            return self.outputs.clone();
        }
        let mut found = Vec::new();
        for node in &self.graph.nodes {
            let tool =
                (tools.iter()).find(|tool| tool.id == node.tool && tool.version == node.version);
            for output in tool.iter().flat_map(|tool| &tool.outputs) {
                let read = (self.graph.edges.iter())
                    .any(|edge| edge.from.node == node.id && edge.from.port == output.name);
                if !read {
                    found.push(ComposedOutput {
                        name: format!("{}.{}", node.id, output.name),
                        description: output.description.clone(),
                        from: End {
                            node: node.id.clone(),
                            port: output.name.clone(),
                        },
                    });
                }
            }
        }
        found
    }

    /// The `.uwe.yaml` text of the workflow.
    pub fn to_yaml(&self) -> String {
        let header = "# A workflow composed of tools, as `uwe run` runs it.\n";
        format!("{header}{}", tool_definition::yaml::write(&self.to_json()))
    }

    /// The JSON form of the `.uwe.yaml` file.
    pub fn to_json(&self) -> Json {
        let mut object = Map::new();
        object.insert("uwe_version".into(), json!(VERSION));
        object.insert("name".into(), json!(self.name));
        if let Some(text) = &self.description {
            object.insert("description".into(), json!(text));
        }
        let mut tools: Vec<Json> = self.references.iter().map(|path| json!(path)).collect();
        tools.extend(
            self.tools
                .iter()
                .filter_map(|tool| serde_json::to_value(tool).ok()),
        );
        if !tools.is_empty() {
            object.insert("tools".into(), Json::Array(tools));
        }
        let mut inputs = Map::new();
        for input in &self.inputs {
            inputs.insert(input.name.clone(), input_json(input));
        }
        if !inputs.is_empty() {
            object.insert("inputs".into(), Json::Object(inputs));
        }
        let mut steps = Map::new();
        for node in &self.graph.nodes {
            let mut step = Map::new();
            step.insert("tool".into(), json!(node.tool));
            if !node.version.is_empty() {
                step.insert("version".into(), json!(node.version));
            }
            let mut wired = Map::new();
            for input in &self.inputs {
                for end in input.to.iter().filter(|end| end.node == node.id) {
                    wired.insert(end.port.clone(), json!(input.name));
                }
            }
            for edge in self
                .graph
                .edges
                .iter()
                .filter(|edge| edge.to.node == node.id)
            {
                let from = json!(format!("{}/{}", edge.from.node, edge.from.port));
                let asserted: Vec<Json> = (edge.facts.iter())
                    .filter(|fact| fact.holds && fact.evidence == Evidence::Asserted)
                    .filter_map(|fact| serde_json::to_value(&fact.condition).ok())
                    .collect();
                let value = match asserted.is_empty() {
                    true => from,
                    false => json!({"from": from, "asserts": asserted}),
                };
                wired.insert(edge.to.port.clone(), value);
            }
            if !wired.is_empty() {
                step.insert("in".into(), Json::Object(wired));
            }
            if let Json::Object(values) = &node.parameters
                && !values.is_empty()
            {
                step.insert("parameters".into(), node.parameters.clone());
            }
            steps.insert(node.id.clone(), Json::Object(step));
        }
        object.insert("steps".into(), Json::Object(steps));
        let mut outputs = Map::new();
        for output in &self.outputs {
            let from = json!(format!("{}/{}", output.from.node, output.from.port));
            let value = match &output.description {
                Some(text) => json!({"from": from, "description": text}),
                None => from,
            };
            outputs.insert(output.name.clone(), value);
        }
        if !outputs.is_empty() {
            object.insert("outputs".into(), Json::Object(outputs));
        }
        Json::Object(object)
    }
}

/// Reads the text of a `.uwe.yaml` file. Nodes without a version have an empty one.
pub fn parse(text: &str) -> Result<ComposedWorkflow, String> {
    let value = tool_definition::yaml::read(text)?;
    from_json(&value)
}

/// Reads the JSON form of a `.uwe.yaml` file.
pub fn from_json(value: &Json) -> Result<ComposedWorkflow, String> {
    let object = value
        .as_object()
        .ok_or("a composed workflow is a mapping")?;
    match object.get("uwe_version") {
        Some(version) if version.as_str() == Some(VERSION) || *version == json!(1) => {}
        Some(version) => {
            return Err(format!(
                "uwe_version {version} is not known; this reader knows {VERSION}"
            ));
        }
        None => return Err("the workflow has no uwe_version".into()),
    }
    let known = [
        "uwe_version",
        "name",
        "description",
        "tools",
        "inputs",
        "steps",
        "outputs",
    ];
    if let Some(key) = object.keys().find(|key| !known.contains(&key.as_str())) {
        return Err(format!("unknown key {key}"));
    }
    let text = |key: &str| object.get(key).and_then(Json::as_str).map(str::to_owned);
    let mut workflow = ComposedWorkflow {
        name: text("name").unwrap_or_default(),
        description: text("description"),
        ..ComposedWorkflow::default()
    };
    for tool in (object.get("tools").and_then(Json::as_array))
        .into_iter()
        .flatten()
    {
        match tool {
            Json::String(path) => workflow.references.push(path.clone()),
            other => workflow.tools.push(
                serde_json::from_value(other.clone())
                    .map_err(|error| format!("a tool: {error}"))?,
            ),
        }
    }
    let inputs = object
        .get("inputs")
        .and_then(Json::as_object)
        .cloned()
        .unwrap_or_default();
    for (name, spec) in &inputs {
        workflow.inputs.push(input(name, spec)?);
    }
    let steps = object
        .get("steps")
        .and_then(Json::as_object)
        .ok_or("the workflow has no steps")?;
    for (id, step) in steps {
        let step = step
            .as_object()
            .ok_or_else(|| format!("step {id} is not a mapping"))?;
        let known = ["tool", "version", "in", "parameters"];
        if let Some(key) = step.keys().find(|key| !known.contains(&key.as_str())) {
            return Err(format!("step {id}: unknown key {key}"));
        }
        let tool = (step.get("tool").and_then(Json::as_str))
            .ok_or_else(|| format!("step {id} names no tool"))?;
        let version = match step.get("version") {
            Some(Json::String(version)) => version.clone(),
            Some(Json::Number(number)) => number.to_string(),
            _ => String::new(),
        };
        workflow.graph.nodes.push(Node {
            id: id.clone(),
            tool: tool.to_owned(),
            version,
            parameters: step.get("parameters").cloned().unwrap_or(Json::Null),
            given: Default::default(),
        });
        for (port, from) in step
            .get("in")
            .and_then(Json::as_object)
            .into_iter()
            .flatten()
        {
            let (from, facts) = source(id, port, from)?;
            let to = End {
                node: id.clone(),
                port: port.clone(),
            };
            match from.split_once('/') {
                Some((node, output)) => workflow.graph.edges.push(Edge {
                    from: End {
                        node: node.to_owned(),
                        port: output.to_owned(),
                    },
                    to,
                    facts,
                }),
                None if !facts.is_empty() => {
                    return Err(format!(
                        "step {id}: input {port} asserts conditions of a workflow input"
                    ));
                }
                None => {
                    let input = (workflow.inputs.iter_mut())
                        .find(|input| input.name == from)
                        .ok_or_else(|| format!("step {id}: there is no workflow input {from}"))?;
                    input.to.push(to);
                }
            }
        }
    }
    let outputs = object
        .get("outputs")
        .and_then(Json::as_object)
        .cloned()
        .unwrap_or_default();
    for (name, spec) in &outputs {
        let (from, description) = match spec {
            Json::String(from) => (from.clone(), None),
            Json::Object(fields) => (
                (fields.get("from").and_then(Json::as_str))
                    .ok_or_else(|| format!("output {name} names no source"))?
                    .to_owned(),
                fields
                    .get("description")
                    .and_then(Json::as_str)
                    .map(str::to_owned),
            ),
            _ => return Err(format!("output {name} is neither a source nor a mapping")),
        };
        let (node, port) = from
            .split_once('/')
            .ok_or_else(|| format!("output {name}: {from} is not <step>/<output>"))?;
        workflow.outputs.push(ComposedOutput {
            name: name.clone(),
            description,
            from: End {
                node: node.to_owned(),
                port: port.to_owned(),
            },
        });
    }
    Ok(workflow)
}

/// The source of a step input, `<step>/<output>` or a workflow input, and the conditions that
/// the user asserts of its data, from `{from: ..., asserts: [...]}`.
fn source(step: &str, port: &str, value: &Json) -> Result<(String, Vec<Fact>), String> {
    let fields = match value {
        Json::String(from) => return Ok((from.clone(), Vec::new())),
        Json::Object(fields) => fields,
        _ => return Err(format!("step {step}: input {port} names no source")),
    };
    if let Some(key) = (fields.keys()).find(|key| !["from", "asserts"].contains(&key.as_str())) {
        return Err(format!("step {step}: input {port}: unknown key {key}"));
    }
    let from = (fields.get("from").and_then(Json::as_str))
        .ok_or_else(|| format!("step {step}: input {port} names no source"))?;
    let asserted = fields.get("asserts").cloned().unwrap_or_else(|| json!([]));
    let conditions: Vec<tool_definition::Condition> = serde_json::from_value(asserted)
        .map_err(|error| format!("step {step}: input {port}: asserts: {error}"))?;
    let facts = (conditions.into_iter())
        .map(|condition| Fact {
            condition,
            holds: true,
            evidence: Evidence::Asserted,
        })
        .collect();
    Ok((from.to_owned(), facts))
}

/// An input from its mapping: `type`, `list`, `format` or `formats`, `required`, `default`
/// and `description`.
fn input(name: &str, spec: &Json) -> Result<ComposedInput, String> {
    let fields = spec
        .as_object()
        .ok_or_else(|| format!("input {name} is not a mapping"))?;
    let known = [
        "type",
        "list",
        "pair",
        "format",
        "formats",
        "required",
        "default",
        "description",
    ];
    if let Some(key) = fields.keys().find(|key| !known.contains(&key.as_str())) {
        return Err(format!("input {name}: unknown key {key}"));
    }
    let kind = match fields.get("type").and_then(Json::as_str).unwrap_or("file") {
        "file" => PortKind::File,
        "directory" => PortKind::Directory,
        "string" => PortKind::Value(ValueType::String),
        "integer" => PortKind::Value(ValueType::Integer),
        "number" => PortKind::Value(ValueType::Number),
        "boolean" => PortKind::Value(ValueType::Boolean),
        other => return Err(format!("input {name}: type {other} is not known")),
    };
    let flag = |key: &str| fields.get(key).and_then(Json::as_bool).unwrap_or(false);
    let cardinality = match (flag("list"), flag("pair")) {
        (true, _) => Cardinality::List,
        (_, true) => Cardinality::Pair,
        _ => Cardinality::One,
    };
    let mut formats: Vec<String> = match fields.get("formats") {
        Some(Json::Array(items)) => items
            .iter()
            .filter_map(Json::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    };
    formats.extend(
        fields
            .get("format")
            .and_then(Json::as_str)
            .map(str::to_owned),
    );
    let default = fields.get("default").cloned();
    Ok(ComposedInput {
        name: name.to_owned(),
        description: fields
            .get("description")
            .and_then(Json::as_str)
            .map(str::to_owned),
        supply: Supply {
            kind,
            shape: Shape {
                cardinality,
                per_sample: false,
            },
            formats,
            facts: Vec::new(),
        },
        required: fields
            .get("required")
            .and_then(Json::as_bool)
            .unwrap_or(default.is_none()),
        default,
        to: Vec::new(),
    })
}

fn input_json(input: &ComposedInput) -> Json {
    let kind = match input.supply.kind {
        PortKind::File => "file",
        PortKind::Directory => "directory",
        PortKind::Value(ValueType::Integer) => "integer",
        PortKind::Value(ValueType::Number) => "number",
        PortKind::Value(ValueType::Boolean) => "boolean",
        PortKind::Value(_) => "string",
    };
    let mut fields = Map::new();
    fields.insert("type".into(), json!(kind));
    match input.supply.shape.cardinality {
        Cardinality::List => {
            fields.insert("list".into(), json!(true));
        }
        Cardinality::Pair => {
            fields.insert("pair".into(), json!(true));
        }
        Cardinality::One => {}
    }
    match input.supply.formats.as_slice() {
        [] => {}
        [one] => {
            fields.insert("format".into(), json!(one));
        }
        many => {
            fields.insert("formats".into(), json!(many));
        }
    }
    if input.required == input.default.is_some() {
        fields.insert("required".into(), json!(input.required));
    }
    if let Some(default) = &input.default {
        fields.insert("default".into(), default.clone());
    }
    if let Some(text) = &input.description {
        fields.insert("description".into(), json!(text));
    }
    Json::Object(fields)
}

/// A value of a kind, for the check of a parameter that a workflow input feeds.
fn placeholder(kind: PortKind) -> Json {
    match kind {
        PortKind::Value(ValueType::Integer | ValueType::Number) => json!(0),
        PortKind::Value(ValueType::Boolean) => json!(false),
        PortKind::Value(ValueType::Array) => json!([]),
        PortKind::Value(ValueType::Object) => json!({}),
        _ => json!(""),
    }
}

/// The wir document of a YAML file with a `wir_version`, as a JSON wir document reads.
pub fn wir_document(text: &str) -> Result<Option<wir::Document>, String> {
    let value = tool_definition::yaml::read(text)?;
    if value.get("wir_version").is_none() {
        return Ok(None);
    }
    let document = wir::parse(&value.to_string()).map_err(|error| error.to_string())?;
    Ok(Some(document))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = r#"
uwe_version: "1"
name: qc
description: Counts the lines of each file.
tools:
  - tools/count.xml
inputs:
  texts:
    type: file
    list: true
    format: format_2330
    description: The files to count.
  header:
    type: boolean
    default: false
steps:
  count:
    tool: count_lines
    in:
      input: texts
      header: header
  report:
    tool: report
    version: "2"
    in:
      counts: count/output
    parameters:
      title: Counts
outputs:
  summary:
    from: report/html
    description: The report.
"#;

    #[test]
    fn reads_and_writes() {
        let workflow = parse(TEXT).expect("a composed workflow");
        assert_eq!(workflow.references, ["tools/count.xml"]);
        assert_eq!(
            workflow.inputs[0].supply.shape.cardinality,
            Cardinality::List
        );
        assert!(workflow.inputs[0].required && !workflow.inputs[1].required);
        assert_eq!(
            workflow.inputs[0].to,
            [End {
                node: "count".into(),
                port: "input".into()
            }]
        );
        assert_eq!(workflow.graph.edges[0].from.port, "output");
        assert_eq!(workflow.graph.nodes[0].version, "", "the library picks it");
        assert_eq!(workflow.outputs[0].from.node, "report");
        let written = workflow.to_yaml();
        assert_eq!(parse(&written), Ok(workflow), "{written}");
    }

    #[test]
    fn keeps_asserted_conditions() {
        let text = TEXT.replace(
            "counts: count/output",
            "counts:\n        from: count/output\n        asserts: [indexed, {sorted: coordinate}]",
        );
        let workflow = parse(&text).expect("a composed workflow");
        let conditions: Vec<_> = (workflow.graph.edges[0].facts.iter())
            .map(|fact| (fact.condition.clone(), fact.evidence.clone()))
            .collect();
        assert_eq!(
            conditions,
            [
                (tool_definition::Condition::Indexed, Evidence::Asserted),
                (
                    tool_definition::Condition::Sorted(tool_definition::SortKey::Coordinate),
                    Evidence::Asserted
                )
            ]
        );
        assert_eq!(parse(&workflow.to_yaml()), Ok(workflow));
        let misplaced = TEXT.replace("input: texts", "input: {from: texts, asserts: [indexed]}");
        assert!(
            parse(&misplaced)
                .unwrap_err()
                .contains("asserts conditions")
        );
    }

    #[test]
    fn refuses_unknown_keys() {
        let text = TEXT.replace("description: Counts", "summary: x\ndescription: Counts");
        assert_eq!(parse(&text).unwrap_err(), "unknown key summary");
        assert!(parse("uwe_version: \"9\"\nsteps: {}\n").is_err());
        let missing = TEXT.replace("input: texts", "input: nothing");
        assert!(
            parse(&missing)
                .unwrap_err()
                .contains("no workflow input nothing")
        );
    }
}
