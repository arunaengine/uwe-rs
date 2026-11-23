//! Tool definitions of CWL CommandLineTools: File and Directory inputs and outputs as ports
//! with their EDAM formats, other inputs as parameters, and the image and resources asked for.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use cwl_parser::model::{Class, Dynamic, Expression, Part, RequirementClass, Type, short_name};
use serde_json::{Map, Value as Json, json};
use tool_definition::edam::term_id;
use tool_definition::{
    Cardinality, Condition, Curation, Definition, Image, InputPort, OutputFormat, OutputPort,
    PortKind, Resources, SourceKind, ValueType,
};

/// The definition of the CommandLineTool in the file, or of each CommandLineTool of a packed
/// document's `$graph`; other process classes are no tools.
pub(crate) fn definitions(path: &Path) -> Result<Vec<Definition>, String> {
    let mut loader = cwl_parser::Loader::new(cwl_parser::source::FileSystem);
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    match packed(&text)? {
        None => Ok(vec![definition(&mut loader, path, None)?]),
        Some(ids) if ids.is_empty() => {
            Err("the packed CWL document has no CommandLineTool".to_owned())
        }
        Some(ids) => (ids.iter())
            .map(|id| definition(&mut loader, path, Some(id)))
            .collect(),
    }
}

/// The ids of the CommandLineTools of a packed document, without their `#`; none for a
/// document without `$graph`.
fn packed(text: &str) -> Result<Option<Vec<String>>, String> {
    if !text.contains("$graph") {
        return Ok(None);
    }
    let value = tool_definition::yaml::read(text)?;
    let Some(graph) = value.get("$graph").and_then(Json::as_array) else {
        return Ok(None);
    };
    let tools = (graph.iter())
        .filter(|entry| entry.get("class").and_then(Json::as_str) == Some("CommandLineTool"))
        .filter_map(|entry| entry.get("id").and_then(Json::as_str))
        .map(|id| id.rsplit('#').next().unwrap_or(id).to_owned());
    Ok(Some(tools.collect()))
}

/// The definition of the CommandLineTool of a file, or of the process `fragment` of its graph.
fn definition(
    loader: &mut cwl_parser::Loader,
    path: &Path,
    fragment: Option<&str>,
) -> Result<Definition, String> {
    let shown = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    let mut reference = shown.to_string_lossy().into_owned();
    if let Some(fragment) = fragment {
        reference = format!("{reference}#{fragment}");
    }
    let document = loader.load(&reference).map_err(|error| error.to_string())?;
    let process = &document.process;
    let class = match &process.class {
        Class::CommandLineTool(_) => None,
        Class::ExpressionTool(_) => Some("ExpressionTool"),
        Class::Workflow(_) => Some("Workflow"),
        Class::Operation => Some("Operation"),
    };
    if let Some(class) = class {
        return Err(format!("a CWL {class} is not a tool"));
    }
    let requirements: Vec<&RequirementClass> = (process.requirements.iter())
        .chain(&process.hints)
        .map(|requirement| &requirement.class)
        .collect();
    let mut image = None;
    let mut resources = Resources::default();
    let mut version = None;
    for requirement in &requirements {
        match requirement {
            RequirementClass::Docker(docker) if image.is_none() => {
                image = (docker.docker_pull.clone()).map(|name| Image { name, digest: None });
            }
            RequirementClass::Resource(resource) => {
                if let Some(Dynamic::Value(cores)) = &resource.cores_min {
                    resources.cpus = Some(cores.ceil() as u32);
                }
                if let Some(Dynamic::Value(mebibytes)) = &resource.ram_min {
                    resources.memory = Some((mebibytes * 1024.0 * 1024.0) as u64);
                }
            }
            RequirementClass::ToolTimeLimit {
                timelimit: Dynamic::Value(seconds),
            } if *seconds > 0 => resources.time = Some(*seconds as u64),
            RequirementClass::Software { packages } if version.is_none() => {
                version = (packages.first()).and_then(|package| package.version.first().cloned());
            }
            _ => {}
        }
    }
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned());
    let id = (fragment.map(str::to_owned))
        .or(stem)
        .unwrap_or_else(|| short_name(&process.id).to_owned());
    let version = version
        .or_else(|| (image.as_ref()).and_then(|image| super::tag(&image.name)))
        .unwrap_or_else(|| "1.0".to_owned());
    let mut inputs = Vec::new();
    let mut properties = Map::new();
    let mut required = Vec::new();
    for input in &process.inputs {
        let name = short_name(&input.id).to_owned();
        let (inner, optional) = unwrapped(&input.value_type);
        let default = (input.default.as_ref()).and_then(|node| {
            serde_json::from_str(&cwl_runtime::Value::from_node(node).to_json()).ok()
        });
        let description = Some(input.doc.join("\n")).filter(|text| !text.is_empty());
        match file_kind(inner) {
            Some((kind, cardinality)) => inputs.push(InputPort {
                name,
                title: input.label.clone(),
                description,
                kind,
                cardinality,
                formats: input.format.iter().filter_map(literal).map(edam).collect(),
                unit: None,
                required: !optional,
                default,
                examples: Vec::new(),
                conditions: match input.secondary_files.is_empty() {
                    true => Vec::new(),
                    false => vec![Condition::Indexed],
                },
                active_when: None,
            }),
            None => {
                let mut schema = value_schema(inner);
                if let Some(label) = &input.label {
                    schema["title"] = json!(label);
                }
                if let Some(text) = description {
                    schema["description"] = json!(text);
                }
                match default {
                    Some(default) => schema["default"] = default,
                    None if !optional => required.push(json!(name)),
                    None => {}
                }
                properties.insert(name, schema);
            }
        }
    }
    let outputs = (process.outputs.iter())
        .map(|output| {
            let (inner, optional) = unwrapped(&output.value_type);
            let (kind, cardinality) = file_kind(inner).unwrap_or_else(|| {
                let value = value_type(inner);
                (PortKind::Value(value), Cardinality::One)
            });
            let format = output.format.as_ref().and_then(literal).map(edam);
            let schema = match kind {
                PortKind::Value(_) => value_schema(inner),
                _ => Json::Null,
            };
            OutputPort {
                name: short_name(&output.id).to_owned(),
                title: output.label.clone(),
                description: Some(output.doc.join("\n")).filter(|text| !text.is_empty()),
                kind,
                cardinality,
                format: OutputFormat::Terms(format.into_iter().collect()),
                guarantees: Vec::new(),
                created_when: None,
                optional,
                schema,
            }
        })
        .collect();
    let mut parameters = Json::Null;
    if !properties.is_empty() {
        parameters = json!({"type": "object", "properties": properties});
        if !required.is_empty() {
            parameters["required"] = Json::Array(required);
        }
    }
    // A tool of a packed document names its process after the file, as a run takes it.
    let mut source = super::source(SourceKind::Cwl, path)?;
    if let Some(fragment) = fragment {
        source.address = format!("{}#{fragment}", source.address);
    }
    Ok(Definition {
        title: process.label.clone().unwrap_or_else(|| id.clone()),
        id,
        version,
        description: Some(process.doc.join("\n")).filter(|text| !text.is_empty()),
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
        outputs,
        resources,
        tests: Vec::new(),
        curation: Curation::Unchecked,
    })
}

/// The type without `null`, and whether `null` was one of its members.
fn unwrapped(value_type: &Type) -> (&Type, bool) {
    match value_type {
        Type::Union(members) => {
            let rest: Vec<&Type> = members
                .iter()
                .filter(|m| !matches!(m, Type::Null))
                .collect();
            let optional = rest.len() < members.len();
            match rest.as_slice() {
                [one] => (one, optional),
                _ => (value_type, optional),
            }
        }
        other => (other, false),
    }
}

/// The port kind of a File or Directory type, or of an array of them.
fn file_kind(value_type: &Type) -> Option<(PortKind, Cardinality)> {
    match value_type {
        Type::File | Type::Stdin | Type::Stdout | Type::Stderr => {
            Some((PortKind::File, Cardinality::One))
        }
        Type::Directory => Some((PortKind::Directory, Cardinality::One)),
        Type::Array(array) => match file_kind(unwrapped(&array.items).0) {
            Some((kind, Cardinality::One)) => Some((kind, Cardinality::List)),
            _ => None,
        },
        _ => None,
    }
}

fn value_type(value_type: &Type) -> ValueType {
    match value_type {
        Type::Boolean => ValueType::Boolean,
        Type::Int | Type::Long => ValueType::Integer,
        Type::Float | Type::Double => ValueType::Number,
        Type::Array(_) => ValueType::Array,
        Type::Record(_) => ValueType::Object,
        _ => ValueType::String,
    }
}

/// The JSON Schema of the values of a type.
fn value_schema(value_type: &Type) -> Json {
    match value_type {
        Type::Boolean => json!({"type": "boolean"}),
        Type::Int | Type::Long => json!({"type": "integer"}),
        Type::Float | Type::Double => json!({"type": "number"}),
        Type::String => json!({"type": "string"}),
        Type::Enum(schema) => {
            let symbols: Vec<&str> = schema.symbols.iter().map(|s| short_name(s)).collect();
            json!({"type": "string", "enum": symbols})
        }
        Type::Array(array) => {
            json!({"type": "array", "items": value_schema(unwrapped(&array.items).0)})
        }
        Type::Record(record) => {
            let fields: Map<String, Json> = (record.fields.iter())
                .map(|field| {
                    let schema = value_schema(unwrapped(&field.value_type).0);
                    (field.name.clone(), schema)
                })
                .collect();
            json!({"type": "object", "properties": fields})
        }
        _ => json!({}),
    }
}

/// The text of an expression without code, like a format IRI.
fn literal(expression: &Expression) -> Option<String> {
    let literal = (expression.parts.iter()).all(|part| matches!(part, Part::Text(_)));
    literal.then(|| expression.text.clone())
}

/// An EDAM term as its short id; other IRIs stay as they are.
fn edam(format: String) -> String {
    term_id(&format).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACKED: &str = r##"cwlVersion: v1.2
$graph:
  - id: sort
    class: CommandLineTool
    baseCommand: [sort]
    inputs:
      text:
        type: File
        inputBinding: {position: 1}
    stdout: sorted.txt
    outputs:
      sorted: stdout
  - id: count
    class: CommandLineTool
    baseCommand: [wc, -l]
    inputs:
      text:
        type: File
        inputBinding: {position: 1}
    stdout: count.txt
    outputs:
      count: stdout
  - id: main
    class: Workflow
    inputs:
      text: File
    outputs:
      count:
        type: File
        outputSource: count/count
    steps:
      sort:
        run: "#sort"
        in: {text: text}
        out: [sorted]
      count:
        run: "#count"
        in: {text: sort/sorted}
        out: [count]
"##;

    #[test]
    fn reads_packed_tools() {
        let folder = tempfile::tempdir().expect("a folder");
        let file = folder.path().join("packed.cwl");
        std::fs::write(&file, PACKED).expect("writes the document");
        let found = definitions(&file).expect("definitions");
        let shown: Vec<(&str, String)> = (found.iter())
            .map(|tool| {
                let address = tool.source.as_ref().map(|source| source.address.clone());
                (tool.id.as_str(), address.unwrap_or_default())
            })
            .collect();
        let at = |id: &str| format!("{}#{id}", file.display());
        assert_eq!(shown, [("sort", at("sort")), ("count", at("count"))]);
        assert_eq!(found[1].outputs[0].name, "count");
        let workflow = folder.path().join("workflow.cwl");
        let plain = "cwlVersion: v1.2\nclass: Workflow\ninputs: {}\noutputs: {}\nsteps: {}\n";
        std::fs::write(&workflow, plain).expect("writes the workflow");
        assert_eq!(
            definitions(&workflow).unwrap_err(),
            "a CWL Workflow is not a tool"
        );
    }
}
