//! The task explorer and the combiner of the studio: queries of the tool library, suggestions,
//! and checks, exports, imports and runs of composed workflows.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use run_page::{
    ComposerRequest, ExportFormat, ExportRequest, Exported, ImportRequest, Refusal, ScanRequest,
    SuggestMode, SuggestRequest, ToolFilter,
};
use serde_json::{Map, Value, json};
use tool_definition::edam::hierarchy;
use tool_definition::{Curation, Definition, InputPort, OutputFormat, Ranking, Supply};

use super::{OUTSIDE, contained};
use crate::tools::{ComposedWorkflow, Entry, Query, ToolLibrary, composed, ecosystem, lower};

/// The tools that match the filter, with the counts of every ecosystem and curation level of
/// the library, and the files that held no tool.
pub(super) fn tools(library: &ToolLibrary, filter: &ToolFilter) -> Result<Value, Refusal> {
    let text = |given: &Option<String>| {
        (given.as_deref())
            .filter(|text| !text.trim().is_empty())
            .map(str::to_owned)
    };
    let curation = match text(&filter.curation).as_deref() {
        None => None,
        Some("curated") => Some(Curation::Curated),
        Some("imported") => Some(Curation::Imported),
        Some("unchecked") => Some(Curation::Unchecked),
        Some(other) => return Err(Refusal::invalid(format!("unknown curation {other}"))),
    };
    let query = Query {
        text: text(&filter.q),
        source_kind: None,
        operation: text(&filter.operation),
        topic: text(&filter.topic),
        input_format: text(&filter.input_format),
        output_format: text(&filter.output_format),
        curation,
    };
    let kind = text(&filter.kind).filter(|kind| kind != "all");
    let found: Vec<&Entry> = (library.query(&query).into_iter())
        .filter(|entry| {
            kind.as_deref()
                .is_none_or(|kind| ecosystem(&entry.definition) == kind)
        })
        .collect();
    let mut ecosystems: BTreeMap<&str, u64> = BTreeMap::new();
    let mut curations: BTreeMap<String, u64> = BTreeMap::new();
    for entry in library.entries() {
        *ecosystems.entry(ecosystem(&entry.definition)).or_default() += 1;
        let level = serde_json::to_value(entry.definition.curation).unwrap_or_default();
        *curations
            .entry(level.as_str().unwrap_or_default().to_owned())
            .or_default() += 1;
    }
    let shown: Vec<&Definition> = found.iter().map(|entry| &entry.definition).collect();
    Ok(json!({
        "tools": found.iter().map(|entry| summary(entry)).collect::<Vec<_>>(),
        "count": library.entries().len(),
        "facets": {"ecosystems": ecosystems, "curation": curations},
        "labels": labels(&shown),
        "skipped": library.skipped(),
    }))
}

/// A tool as the explorer and the canvas show it: its ports with their formats, without the
/// schema of its parameters.
fn summary(entry: &Entry) -> Value {
    let tool = &entry.definition;
    let inputs: Vec<Value> = (tool.inputs.iter())
        .map(|port| {
            json!({
                "name": port.name, "title": port.title, "kind": port.kind,
                "cardinality": port.cardinality, "formats": port.formats,
                "required": port.required, "conditions": port.conditions,
            })
        })
        .collect();
    let outputs: Vec<Value> = (tool.outputs.iter())
        .map(|port| {
            let (formats, follows) = match &port.format {
                OutputFormat::Terms(terms) => (terms.clone(), None),
                OutputFormat::Follows(input) => (Vec::new(), Some(input.clone())),
            };
            json!({
                "name": port.name, "title": port.title, "kind": port.kind,
                "cardinality": port.cardinality, "formats": formats, "follows": follows,
            })
        })
        .collect();
    let parameters =
        (tool.parameters.get("properties").and_then(Value::as_object)).map_or(0, Map::len);
    json!({
        "id": tool.id,
        "version": tool.version,
        "title": tool.title,
        "description": tool.description,
        "ecosystem": ecosystem(tool),
        "curation": tool.curation,
        "operations": tool.operations,
        "topics": tool.topics,
        "keywords": tool.keywords,
        "inputs": inputs,
        "outputs": outputs,
        "parameters": parameters,
        "image": tool.image.as_ref().map(|image| &image.name),
        "origin": entry.origin,
    })
}

/// The EDAM labels of the formats, operations and topics of some tools, by term.
fn labels(tools: &[&Definition]) -> BTreeMap<String, String> {
    let edam = hierarchy();
    let mut terms: BTreeSet<&str> = BTreeSet::new();
    for tool in tools {
        terms.extend(
            tool.operations
                .iter()
                .chain(&tool.topics)
                .map(String::as_str),
        );
        for port in &tool.inputs {
            terms.extend(port.formats.iter().map(String::as_str));
        }
        for port in &tool.outputs {
            if let OutputFormat::Terms(formats) = &port.format {
                terms.extend(formats.iter().map(String::as_str));
            }
        }
    }
    (terms.into_iter())
        .filter_map(|term| {
            let id = tool_definition::edam::term_id(term);
            Some((term.to_owned(), edam.label(id)?.to_owned()))
        })
        .collect()
}

/// The whole definition of a tool version, with where it came from and the labels of its terms.
pub(super) fn tool(library: &ToolLibrary, id: &str, version: &str) -> Result<Value, Refusal> {
    let entry = (library.get(id, Some(version)))
        .ok_or_else(|| Refusal::missing(format!("the library has no tool {id} {version}")))?;
    Ok(json!({
        "definition": entry.definition,
        "ecosystem": ecosystem(&entry.definition),
        "origin": entry.origin,
        "labels": labels(&[&entry.definition]),
    }))
}

/// Adds the tools of a file or of every file below a folder to the library.
pub(super) fn scan(library: &mut ToolLibrary, request: &ScanRequest) -> Result<Value, Refusal> {
    if request.path.trim().is_empty() {
        return Err(Refusal::invalid(
            "give the path of a file or folder of tools",
        ));
    }
    let path = PathBuf::from(&request.path);
    if !path.exists() {
        return Err(Refusal::missing(format!("there is no {}", path.display())));
    }
    let (before, skipped) = (library.entries().len(), library.skipped().len());
    if path.is_dir() {
        library.scan(&path);
    } else {
        library
            .add_file(&path)
            .map_err(|reason| Refusal::failed(format!("{}: {reason}", path.display())))?;
    }
    let added: Vec<Value> = library.entries()[before..].iter().map(summary).collect();
    Ok(json!({
        "count": library.entries().len(),
        "added": added,
        "skipped": &library.skipped()[skipped..],
    }))
}

/// The tools that can follow an output (`followers`) or feed an input (`producers`). The port
/// is a tool port by `tool`, `version` and `port`, or a `supply` or `port` object as given.
pub(super) fn suggest(library: &ToolLibrary, request: &SuggestRequest) -> Result<Value, Refusal> {
    let ranking = Ranking {
        operations: request.operations.clone(),
        uses: BTreeMap::new(),
    };
    let suggestions = match request.mode {
        SuggestMode::Followers => {
            let supply = match &request.supply {
                Some(supply) => serde_json::from_value(supply.clone())
                    .map_err(|error| Refusal::invalid(format!("supply: {error}")))?,
                None => {
                    let (tool, port) = named(library, request)?;
                    Supply::output(tool, port, &BTreeMap::new(), false).ok_or_else(|| {
                        Refusal::missing(format!("the tool {} has no output {port}", tool.id))
                    })?
                }
            };
            library.suggest_followers(&supply, &ranking)
        }
        SuggestMode::Producers => {
            let port: InputPort = match &request.port {
                Some(port @ Value::Object(_)) => serde_json::from_value(port.clone())
                    .map_err(|error| Refusal::invalid(format!("port: {error}")))?,
                _ => {
                    let (tool, name) = named(library, request)?;
                    (tool.input(name).cloned()).ok_or_else(|| {
                        Refusal::missing(format!("the tool {} has no input {name}", tool.id))
                    })?
                }
            };
            library.suggest_producers(&port, &ranking)
        }
    };
    let enriched: Vec<Value> = (suggestions.iter())
        .map(|suggestion| {
            let entry = library.get(&suggestion.tool, Some(&suggestion.version));
            let tool = entry.map(|entry| &entry.definition);
            json!({
                "tool": suggestion.tool,
                "version": suggestion.version,
                "port": suggestion.port,
                "check": suggestion.check,
                "title": tool.map(|tool| &tool.title),
                "ecosystem": tool.map(ecosystem),
                "curation": tool.map(|tool| tool.curation),
                "summary": entry.map(summary),
            })
        })
        .collect();
    Ok(json!({"suggestions": enriched}))
}

/// The tool and the name of the port that a suggestion names.
fn named<'a>(
    library: &'a ToolLibrary,
    request: &'a SuggestRequest,
) -> Result<(&'a Definition, &'a str), Refusal> {
    let port = request.port.as_ref().and_then(Value::as_str);
    let (Some(id), Some(port)) = (request.tool.as_deref(), port) else {
        return Err(Refusal::invalid(
            "name the tool and port, or give the port itself",
        ));
    };
    let entry = (library.get(id, request.version.as_deref()))
        .ok_or_else(|| Refusal::missing(format!("the library has no tool {id}")))?;
    Ok((&entry.definition, port))
}

/// A workflow in the JSON form of a `.uwe.yaml` file, with each node without a version at the
/// last version of its tool. The library is borrowed, unless the workflow holds tools itself.
fn resolved<'a>(
    library: &'a ToolLibrary,
    value: &Value,
) -> Result<(ComposedWorkflow, Cow<'a, ToolLibrary>), Refusal> {
    let mut workflow = composed::from_json(value).map_err(Refusal::invalid)?;
    let mut library = Cow::Borrowed(library);
    if !workflow.tools.is_empty() {
        let owned = library.to_mut();
        for tool in &workflow.tools {
            owned.add(tool.clone(), None);
        }
    }
    for node in &mut workflow.graph.nodes {
        if node.version.is_empty()
            && let Some(entry) = library.get(&node.tool, None)
        {
            node.version.clone_from(&entry.definition.version);
        }
    }
    Ok((workflow, library))
}

/// The findings of every node and edge of a composed workflow, with the ends of each edge in
/// the order of the report, and the version that each node got.
pub(super) fn check(library: &ToolLibrary, request: &ComposerRequest) -> Result<Value, Refusal> {
    let (workflow, library) = resolved(library, &request.workflow)?;
    let tools = workflow.used_tools(&library);
    let graph = workflow.graph_with(&tools);
    let report = tool_definition::graph::check(&graph, &tools);
    let edges: Vec<Value> = (graph.edges.iter())
        .map(|edge| json!({"from": edge.from, "to": edge.to}))
        .collect();
    let versions: Map<String, Value> = (workflow.graph.nodes.iter())
        .map(|node| (node.id.clone(), json!(node.version)))
        .collect();
    Ok(json!({"report": report, "edges": edges, "versions": versions}))
}

/// A composed workflow as `.uwe.yaml` text, as JSON, or lowered into a verified wir document.
pub(super) fn export(
    library: &ToolLibrary,
    folder: &Path,
    request: &ExportRequest,
) -> Result<Exported, Refusal> {
    let (workflow, library) = resolved(library, &request.workflow)?;
    let stem = stem(&workflow);
    let pretty = |value: &Value| serde_json::to_string_pretty(value).unwrap_or_default();
    let (text, file) = match request.format {
        ExportFormat::Yaml => (workflow.to_yaml(), format!("{stem}.uwe.yaml")),
        ExportFormat::Json => (pretty(&workflow.to_json()), format!("{stem}.uwe.json")),
        ExportFormat::Wir => {
            let generated = folder.join("generated").join(&stem);
            let lowered = lower(&workflow, &library, (folder, &generated))
                .map_err(|error| Refusal::failed(error.to_string()))?;
            crate::tools::write_files(&lowered.files).map_err(Refusal::failed)?;
            let document = serde_json::to_value(&lowered.document).unwrap_or_default();
            (pretty(&document), format!("{stem}.wir.json"))
        }
    };
    Ok(Exported { text, file })
}

/// The stem of the files of a workflow, from its name.
fn stem(workflow: &ComposedWorkflow) -> String {
    match workflow.name.trim() {
        "" => "workflow".to_owned(),
        name => crate::tools::lowering::name(name),
    }
}

/// Reads a `.uwe.yaml` text, or its JSON form, and adds the tools it names below `base`, or
/// below `folders` when it names no base, and the tools it holds, to the library. An isolated
/// server reads only files inside its `roots`.
pub(super) fn import(
    library: &mut ToolLibrary,
    (folders, roots): (&[PathBuf], Option<&[PathBuf]>),
    request: &ImportRequest,
) -> Result<Value, Refusal> {
    let workflow = composed::parse(&request.text).map_err(Refusal::invalid)?;
    let bases: Vec<PathBuf> = match &request.base {
        Some(base) => vec![PathBuf::from(base)],
        None => folders.to_vec(),
    };
    let before = library.entries().len();
    for tool in &workflow.tools {
        library.add(tool.clone(), None);
    }
    let mut missing = Vec::new();
    for reference in &workflow.references {
        let found = bases
            .iter()
            .map(|base| base.join(reference))
            .find(|path| path.exists());
        match found {
            Some(path) if roots.is_some_and(|roots| !contained(&path, roots)) => {
                missing.push(json!({"reference": reference, "reason": OUTSIDE}));
            }
            Some(path) if path.is_dir() => {
                library.scan(&path);
            }
            Some(path) => {
                if let Err(reason) = library.add_file(&path) {
                    missing.push(json!({"reference": reference, "reason": reason}));
                }
            }
            None => missing.push(json!({"reference": reference, "reason": "not found"})),
        }
    }
    Ok(json!({
        "workflow": workflow.to_json(),
        "added": library.entries().len() - before,
        "missing": missing,
    }))
}

/// Writes a composed workflow below `folder` as a `.uwe.yaml` file that holds every tool it
/// uses, so that a run lowers it as `uwe run` lowers any composed workflow. Its path.
pub(super) fn prepared(
    library: &ToolLibrary,
    folder: &Path,
    request: &ComposerRequest,
) -> Result<PathBuf, Refusal> {
    let (mut workflow, library) = resolved(library, &request.workflow)?;
    let lowered = lower(&workflow, &library, (folder, &folder.join("generated")));
    lowered.map_err(|error| Refusal::failed(error.to_string()))?;
    let mut used: Vec<Definition> = Vec::new();
    for node in &workflow.graph.nodes {
        let known = used
            .iter()
            .any(|tool| tool.id == node.tool && tool.version == node.version);
        if let (false, Some(entry)) = (known, library.get(&node.tool, Some(&node.version))) {
            let mut definition = entry.definition.clone();
            // A source relative to the file of the tool must not move with the workflow.
            if let (Some(source), Some(origin)) = (&mut definition.source, &entry.origin)
                && !source.address.contains("://")
                && Path::new(&source.address).is_relative()
                && let Some(parent) = origin.parent()
            {
                source.address = parent.join(&source.address).to_string_lossy().into_owned();
            }
            used.push(definition);
        }
    }
    workflow.tools = used;
    workflow.references.clear();
    let text = workflow.to_yaml();
    let digest = task_model::Digest::sha256(text.as_bytes());
    let stem = stem(&workflow);
    let path = folder
        .join("composed")
        .join(format!("{stem}-{}.uwe.yaml", &digest.hex()[..12]));
    crate::tools::write_files(&[(path.clone(), text)]).map_err(Refusal::failed)?;
    Ok(path)
}
