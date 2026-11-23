//! The commands of `uwe tool`: list, inspect, extract and run tools, check composed workflows,
//! and find the tools that fit a port or match some words.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use tool_definition::{
    Cardinality, Definition, Fit, OutputFormat, PortKind, Ranking, SourceKind, Suggestion, Supply,
};

use super::lowering::{fragment, located, name};
use super::{Query, ToolLibrary, composed, ecosystem};
use crate::arguments::Arguments;
use crate::{Failure, failure};

/// The library of the `-tools` files and folders, else of the current folder.
fn library(arguments: &Arguments) -> Result<ToolLibrary, Failure> {
    let paths = match arguments.tools.is_empty() {
        true => vec![PathBuf::from(".")],
        false => arguments.tools.clone(),
    };
    let mut library = ToolLibrary::new();
    for path in &paths {
        match path.is_dir() {
            true => {
                library.scan(path);
            }
            false => {
                let added = library.add_file(path);
                added.map_err(|reason| failure(format!("uwe: {}: {reason}", path.display())))?;
            }
        }
    }
    Ok(library)
}

fn print_json(value: &Value) -> Result<(), Failure> {
    let text = serde_json::to_string_pretty(value).map_err(|error| failure(error.to_string()))?;
    println!("{text}");
    Ok(())
}

/// One line per tool: id, version, ecosystem, the numbers of inputs and outputs, and title.
fn table(tools: &[&Definition]) -> String {
    let mut text = format!(
        "{:<28} {:<12} {:<10} {:>3} {:>3}  TITLE\n",
        "ID", "VERSION", "KIND", "IN", "OUT"
    );
    for tool in tools {
        let _ = writeln!(
            text,
            "{:<28} {:<12} {:<10} {:>3} {:>3}  {}",
            tool.id,
            tool.version,
            ecosystem(tool),
            tool.inputs.len(),
            tool.outputs.len(),
            tool.title
        );
    }
    text
}

/// A short form of a tool for the JSON of `list` and `suggest`.
fn brief(tool: &Definition, origin: Option<&Path>) -> Value {
    json!({
        "id": tool.id, "version": tool.version, "title": tool.title,
        "ecosystem": ecosystem(tool), "curation": tool.curation,
        "inputs": tool.inputs.len(), "outputs": tool.outputs.len(), "origin": origin,
    })
}

/// `uwe tool list`: the tools of the library, and the files it took none from.
pub fn list(arguments: &Arguments) -> Result<(), Failure> {
    if arguments.target.is_some() {
        return Err(Failure::usage(
            "uwe: use `uwe tool list [-tools <folder>]...`",
        ));
    }
    let library = library(arguments)?;
    if arguments.json {
        let tools: Vec<Value> = (library.entries().iter())
            .map(|entry| brief(&entry.definition, entry.origin.as_deref()))
            .collect();
        return print_json(&json!({"tools": tools, "skipped": library.skipped()}));
    }
    let tools: Vec<&Definition> = library.entries().iter().map(|e| &e.definition).collect();
    print!("{}", table(&tools));
    if !library.skipped().is_empty() {
        eprintln!(
            "uwe: {} files held no tool; `uwe tool list -json` says why",
            library.skipped().len()
        );
    }
    Ok(())
}

/// `uwe tool inspect <id> [<version>]`: the whole definition of a tool version.
pub fn inspect(arguments: &Arguments) -> Result<(), Failure> {
    let usage = || Failure::usage("uwe: use `uwe tool inspect <id> [<version>]`");
    let id = arguments
        .target
        .as_ref()
        .ok_or_else(usage)?
        .to_string_lossy();
    let version = match arguments.targets.as_slice() {
        [] => None,
        [version] => Some(version.as_str()),
        _ => return Err(usage()),
    };
    let library = library(arguments)?;
    let entry = (library.get(&id, version))
        .ok_or_else(|| failure(format!("uwe: the library has no tool {id}")))?;
    let tool = &entry.definition;
    if arguments.json {
        let value = serde_json::to_value(tool).map_err(|error| failure(error.to_string()))?;
        return print_json(&value);
    }
    print!("{}", described(tool, entry.origin.as_deref()));
    Ok(())
}

/// A tool as text: its title, source, image, ports with their formats and its parameters.
fn described(tool: &Definition, origin: Option<&Path>) -> String {
    let edam = tool_definition::edam::hierarchy();
    let label = |term: &String| {
        let id = tool_definition::edam::term_id(term);
        edam.label(id)
            .map_or_else(|| term.clone(), |name| format!("{name} ({id})"))
    };
    let mut text = format!("{} {}: {}\n", tool.id, tool.version, tool.title);
    if let Some(description) = &tool.description {
        let _ = writeln!(text, "{description}");
    }
    let _ = writeln!(text, "kind       {}", ecosystem(tool));
    if let Some(origin) = origin {
        let _ = writeln!(text, "file       {}", origin.display());
    }
    if let Some(image) = &tool.image {
        let _ = writeln!(text, "image      {}", image.name);
    }
    if !tool.operations.is_empty() {
        let names: Vec<String> = tool.operations.iter().map(label).collect();
        let _ = writeln!(text, "operations {}", names.join(", "));
    }
    text.push_str("\ninputs:\n");
    for port in &tool.inputs {
        let formats: Vec<String> = port.formats.iter().map(label).collect();
        let required = if port.required { "" } else { " (optional)" };
        let _ = writeln!(text, "  {}{required}: {}", port.name, formats.join(", "));
    }
    text.push_str("outputs:\n");
    for port in &tool.outputs {
        let formats = match &port.format {
            OutputFormat::Terms(terms) => terms.iter().map(label).collect::<Vec<_>>().join(", "),
            OutputFormat::Follows(input) => format!("the format of input {input}"),
        };
        let _ = writeln!(text, "  {}: {formats}", port.name);
    }
    let parameters = (tool.parameters.get("properties").and_then(Value::as_object))
        .map(|properties| properties.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    if !parameters.is_empty() {
        let _ = writeln!(text, "parameters: {}", parameters.join(", "));
    }
    text
}

/// `uwe tool extract <file> [-save <folder>]`: the tool definitions that uwe reads from one
/// file; with `-save`, each one written as a definition file into the folder.
pub fn extract(arguments: &Arguments) -> Result<(), Failure> {
    let path = (arguments.target.as_deref())
        .filter(|_| arguments.targets.is_empty())
        .ok_or_else(|| Failure::usage("uwe: use `uwe tool extract <file> [-save <folder>]`"))?;
    let found = super::extract(path).map_err(|reason| failure(format!("uwe: {reason}")))?;
    if let Some(folder) = &arguments.save {
        let written = save(&found, path, folder)?;
        if arguments.json {
            return print_json(&json!({"written": written}));
        }
        for item in &written {
            let text = |key: &str| item[key].as_str().unwrap_or_default().to_owned();
            println!(
                "uwe: wrote {} {} to {}",
                text("id"),
                text("version"),
                text("file")
            );
        }
        return Ok(());
    }
    if arguments.json {
        let value = serde_json::to_value(&found).map_err(|error| failure(error.to_string()))?;
        return print_json(&value);
    }
    for tool in &found {
        println!("{}", described(tool, Some(path)));
    }
    Ok(())
}

/// Writes each definition into `folder` as `<id>.tool.json`, with a number added where the
/// folder holds another tool of that id, and the source address made absolute, so that the
/// library reads the files from any folder. What it wrote, as JSON.
fn save(found: &[Definition], origin: &Path, folder: &Path) -> Result<Vec<Value>, Failure> {
    let failed = |error: std::io::Error| failure(format!("uwe: {}: {error}", folder.display()));
    std::fs::create_dir_all(folder).map_err(failed)?;
    let mut taken: Vec<String> = Vec::new();
    let mut written = Vec::new();
    for definition in found {
        let mut definition = definition.clone();
        if let Some(source) = &mut definition.source {
            source.address = absolute(&source.address, source.kind, origin);
        }
        let stem = name(&definition.id);
        let mut file = format!("{stem}.tool.json");
        let mut number = 1;
        // A file of the same tool from the same source is replaced; one of another tool stays.
        let other = |file: &str| {
            let text = std::fs::read_to_string(folder.join(file)).ok();
            let saved = text.and_then(|text| serde_json::from_str::<Definition>(&text).ok());
            saved.is_some_and(|saved| {
                (saved.id.as_str(), saved.version.as_str(), &saved.source)
                    != (
                        definition.id.as_str(),
                        definition.version.as_str(),
                        &definition.source,
                    )
            })
        };
        while taken.contains(&file) || other(&file) {
            number += 1;
            file = format!("{stem}-{number}.tool.json");
        }
        taken.push(file.clone());
        let path = folder.join(&file);
        let text = serde_json::to_string_pretty(&definition)
            .map_err(|error| failure(error.to_string()))?;
        std::fs::write(&path, format!("{text}\n")).map_err(failed)?;
        written.push(json!({"id": definition.id, "version": definition.version, "file": path}));
    }
    Ok(written)
}

/// A source address as an absolute path, from the folder of the file that named it, with the
/// process of a packed CWL document kept after its `#`.
fn absolute(address: &str, kind: SourceKind, origin: &Path) -> String {
    let (file, process) = match kind {
        SourceKind::Cwl => fragment(address),
        _ => (address, None),
    };
    let place = located(file, Some(origin));
    let place = std::path::absolute(&place).unwrap_or(place);
    let mut text = place.to_string_lossy().into_owned();
    if let Some(process) = process {
        text = format!("{text}#{process}");
    }
    text
}

/// `uwe tool run <tool or file> [<version>]`: the arguments of `uwe run` for a workflow of one
/// step that runs the tool, written below the cache folder, with the values of its inputs from
/// `--<name> <value>` and `-inputs`. A file names its tools; `<file>#<step>` or `-entry` picks one.
pub fn run_arguments(arguments: &Arguments) -> Result<Arguments, Failure> {
    let usage =
        || Failure::usage("uwe: use `uwe tool run <tool> [<version>]` or `<file>[#<step>]`");
    let target = arguments.target.as_ref().ok_or_else(usage)?;
    let text = target.to_string_lossy().into_owned();
    let (file, step) = match text.rsplit_once('#') {
        Some((file, step)) if Path::new(file).is_file() => (Some(PathBuf::from(file)), Some(step)),
        _ => (Some(target.clone()).filter(|path| path.is_file()), None),
    };
    let (definition, origin) = match file {
        Some(file) => {
            let picked = match (step, arguments.targets.as_slice()) {
                (Some(step), []) => Some(step.to_owned()),
                (None, []) => None,
                (None, [step]) => Some(step.clone()),
                _ => return Err(usage()),
            };
            let found = super::extract(&file)
                .map_err(|reason| failure(format!("uwe: {}: {reason}", file.display())))?;
            (pick(found, picked.as_deref(), &file)?, file)
        }
        None => {
            let version = match arguments.targets.as_slice() {
                [] => None,
                [version] => Some(version.as_str()),
                _ => return Err(usage()),
            };
            let library = library(arguments)?;
            let entry = (library.get(&text, version))
                .ok_or_else(|| failure(format!("uwe: the library has no tool {text}")))?;
            let origin = (entry.origin.clone()).ok_or_else(|| {
                failure(format!(
                    "uwe: the tool {text} comes from no file uwe can name"
                ))
            })?;
            (entry.definition.clone(), origin)
        }
    };
    let supplied = supplied(arguments)?;
    let workflow = workflow(&definition, &origin, &supplied)?;
    let setup = crate::setup::Setup::load(arguments)?;
    let digest = task_model::Digest::sha256(workflow.as_bytes());
    let folder = (setup.cache_directory())
        .join("scratch")
        .join("tools")
        .join(&digest.hex()[..12]);
    let path = folder.join(format!("{}.uwe.yaml", name(&definition.id)));
    super::write_files(&[(path.clone(), workflow)])
        .map_err(|reason| failure(format!("uwe: {reason}")))?;
    Ok(Arguments {
        target: Some(path),
        targets: Vec::new(),
        language: None,
        ..arguments.clone()
    })
}

/// The tool of a file that `step` names by its id or title, else by them in another case, like
/// the name of a Nextflow process; without a name, the one tool of the file.
fn pick(found: Vec<Definition>, step: Option<&str>, file: &Path) -> Result<Definition, Failure> {
    let ids: Vec<String> = found.iter().map(|tool| tool.id.clone()).collect();
    let shown = file.display();
    let Some(step) = step else {
        return match <[Definition; 1]>::try_from(found) {
            Ok([one]) => Ok(one),
            Err(_) => Err(failure(format!(
                "uwe: {shown} holds the tools {}; pick one with `{shown}#<step>` or `-entry <step>`",
                ids.join(", ")
            ))),
        };
    };
    let exact = |tool: &&Definition| tool.id == step || tool.title == step;
    let loose = |tool: &&Definition| {
        tool.id.eq_ignore_ascii_case(step) || tool.title.eq_ignore_ascii_case(step)
    };
    let chosen = (found.iter().find(exact)).or_else(|| found.iter().find(loose));
    chosen.cloned().ok_or_else(|| {
        failure(format!(
            "uwe: {shown} has no tool {step}; it holds {}",
            ids.join(", ")
        ))
    })
}

/// The names that the run gives values to: its `--<name>` params and the keys of `-inputs`.
fn supplied(arguments: &Arguments) -> Result<Vec<String>, Failure> {
    let mut names: Vec<String> = arguments
        .params
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    if let Some(file) = &arguments.inputs {
        let text = std::fs::read_to_string(file)
            .map_err(|error| failure(format!("uwe: {}: {error}", file.display())))?;
        let value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(_) => tool_definition::yaml::read(&text)
                .map_err(|error| failure(format!("uwe: {}: {error}", file.display())))?,
        };
        let Value::Object(object) = value else {
            return Err(failure(format!(
                "uwe: {} must hold a mapping",
                file.display()
            )));
        };
        names.extend(object.keys().cloned());
    }
    Ok(names)
}

/// The `.uwe.yaml` text of a workflow of one step that runs `tool` from `origin`: an input per
/// port that is required or given and per given parameter, and an output per tool output.
fn workflow(tool: &Definition, origin: &Path, supplied: &[String]) -> Result<String, Failure> {
    let given = |name: &str| supplied.iter().any(|known| known == name);
    let properties = (tool.parameters.get("properties").and_then(Value::as_object))
        .cloned()
        .unwrap_or_default();
    let required: Vec<&str> = (tool.parameters.get("required").and_then(Value::as_array))
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let nextflow = (tool.source.as_ref())
        .is_some_and(|source| matches!(source.kind, SourceKind::Nextflow | SourceKind::NfCore));
    let known = |name: &str| {
        tool.input(name).is_some() || properties.contains_key(name) || (nextflow && name == "meta")
    };
    if let Some(unknown) = supplied.iter().find(|name| !known(name)) {
        let mut names: Vec<&str> = tool.inputs.iter().map(|port| port.name.as_str()).collect();
        names.extend(properties.keys().map(String::as_str));
        return Err(failure(format!(
            "uwe: the tool {} has no input {unknown}; it takes {}",
            tool.id,
            names.join(", ")
        )));
    }
    let missing: Vec<&str> = (tool.inputs.iter())
        .filter(|port| port.required && port.default.is_none())
        .map(|port| port.name.as_str())
        .chain(required.iter().copied().filter(|name| {
            properties
                .get(*name)
                .is_none_or(|schema| schema.get("default").is_none())
        }))
        .filter(|name| !given(name))
        .collect();
    if !missing.is_empty() {
        return Err(failure(format!(
            "uwe: the tool {} needs {}; give each as `--<name> <value>` or in `-inputs`",
            tool.id,
            missing.join(", ")
        )));
    }
    let mut inputs = Map::new();
    let mut wired = Map::new();
    for port in tool
        .inputs
        .iter()
        .filter(|port| port.required || given(&port.name))
    {
        let kind = match port.kind {
            PortKind::Directory => "directory",
            _ => "file",
        };
        let mut input = json!({"type": kind});
        match port.cardinality {
            Cardinality::List => input["list"] = json!(true),
            Cardinality::Pair => input["pair"] = json!(true),
            Cardinality::One => {}
        }
        if !port.formats.is_empty() {
            input["formats"] = json!(port.formats);
        }
        if let Some(text) = &port.description {
            input["description"] = json!(text);
        }
        inputs.insert(port.name.clone(), input);
        wired.insert(port.name.clone(), json!(port.name));
    }
    for (key, schema) in properties.iter().filter(|(key, _)| given(key)) {
        let kind = match schema.get("type").and_then(Value::as_str) {
            Some("boolean") => "boolean",
            Some("integer") => "integer",
            Some("number") => "number",
            _ => "string",
        };
        inputs.insert(key.clone(), json!({"type": kind}));
        wired.insert(key.clone(), json!(key));
    }
    if nextflow && given("meta") {
        inputs.insert("meta".into(), json!({"type": "string"}));
        wired.insert("meta".into(), json!("meta"));
    }
    let step = name(&tool.id);
    let outputs: Map<String, Value> = (tool.outputs.iter())
        .map(|port| (port.name.clone(), json!(format!("{step}/{}", port.name))))
        .collect();
    let origin = std::path::absolute(origin).unwrap_or_else(|_| origin.to_owned());
    let mut node = json!({"tool": tool.id, "version": tool.version});
    if !wired.is_empty() {
        node["in"] = Value::Object(wired);
    }
    let mut document = Map::new();
    document.insert("uwe_version".into(), json!(composed::VERSION));
    document.insert("name".into(), json!(step));
    document.insert("tools".into(), json!([origin]));
    if !inputs.is_empty() {
        document.insert("inputs".into(), Value::Object(inputs));
    }
    document.insert("steps".into(), json!({step.clone(): node}));
    if !outputs.is_empty() {
        document.insert("outputs".into(), Value::Object(outputs));
    }
    let document = Value::Object(document);
    Ok(format!(
        "# The tool {} {} as a workflow of one step, made by `uwe tool run`.\n{}",
        tool.id,
        tool.version,
        tool_definition::yaml::write(&document)
    ))
}

/// `uwe tool check <composed workflow>`: the findings of every node and edge; it fails when a
/// node or edge does not fit.
pub fn check(arguments: &Arguments) -> Result<(), Failure> {
    let path = (arguments.target.as_deref())
        .filter(|_| arguments.targets.is_empty())
        .ok_or_else(|| Failure::usage("uwe: use `uwe tool check <file>.uwe.yaml`"))?;
    let mut library = match arguments.tools.is_empty() {
        true => ToolLibrary::new(),
        false => library(arguments)?,
    };
    let workflow =
        composed::load(path, &mut library).map_err(|error| failure(format!("uwe: {error}")))?;
    let tools = workflow.used_tools(&library);
    let report = tool_definition::graph::check(&workflow.graph_with(&tools), &tools);
    if arguments.json {
        let value = serde_json::to_value(&report).map_err(|error| failure(error.to_string()))?;
        print_json(&value)?;
    } else {
        let fit = |fit: Fit| match fit {
            Fit::Fits => "fits",
            Fit::Unknown => "unknown",
            Fit::DoesNotFit => "does not fit",
        };
        for node in &report.nodes {
            let scatter = if node.scatter {
                ", maps over a list"
            } else {
                ""
            };
            println!("step {}: {}{scatter}", node.id, fit(node.fit));
            for finding in &node.findings {
                println!("  {}: {}", fit(finding.fit), finding.reason);
            }
        }
        for (edge, check) in workflow.graph.edges.iter().zip(&report.edges) {
            let (from, to) = (&edge.from, &edge.to);
            let ends = format!("{}/{} -> {}/{}", from.node, from.port, to.node, to.port);
            println!("{ends}: {}", fit(check.fit));
            for finding in &check.findings {
                println!("  {}: {}", fit(finding.fit), finding.reason);
            }
        }
    }
    match report.fit {
        Fit::DoesNotFit => Err(failure("uwe: the workflow does not fit")),
        _ => Ok(()),
    }
}

/// `uwe tool suggest <tool> <port>`: the tools that can read an output of the tool, or feed
/// one of its inputs; other words list the tools that they match.
pub fn suggest(arguments: &Arguments) -> Result<(), Failure> {
    let first = (arguments.target.as_ref())
        .map(|word| word.to_string_lossy().into_owned())
        .ok_or_else(|| Failure::usage("uwe: use `uwe tool suggest <tool> <port>` or `<words>`"))?;
    let library = library(arguments)?;
    let port = match arguments.targets.as_slice() {
        [port] => Some(port.as_str()),
        _ => None,
    };
    let tool = library.get(&first, None).map(|entry| &entry.definition);
    let Some((tool, port)) = tool.zip(port) else {
        let words = std::iter::once(first).chain(arguments.targets.iter().cloned());
        let query = Query {
            text: Some(words.collect::<Vec<_>>().join(" ")),
            ..Query::default()
        };
        let found = library.query(&query);
        if arguments.json {
            let tools: Vec<Value> = (found.iter())
                .map(|entry| brief(&entry.definition, entry.origin.as_deref()))
                .collect();
            return print_json(&json!({"tools": tools}));
        }
        let tools: Vec<&Definition> = found.iter().map(|entry| &entry.definition).collect();
        print!("{}", table(&tools));
        return Ok(());
    };
    let ranking = Ranking::default();
    let (mode, suggestions): (&str, Vec<Suggestion>) = match tool.output(port) {
        Some(_) => {
            let supply = Supply::output(tool, port, &BTreeMap::new(), false)
                .ok_or_else(|| failure(format!("uwe: {} has no output {port}", tool.id)))?;
            ("followers", library.suggest_followers(&supply, &ranking))
        }
        None => {
            let input = (tool.input(port))
                .ok_or_else(|| failure(format!("uwe: {} has no port {port}", tool.id)))?;
            ("producers", library.suggest_producers(input, &ranking))
        }
    };
    if arguments.json {
        let value = json!({"mode": mode, "suggestions": suggestions});
        return print_json(&value);
    }
    let reads = if mode == "followers" {
        "reads"
    } else {
        "feeds"
    };
    println!("tools whose port {reads} {}/{port}, best first:", tool.id);
    for suggestion in &suggestions {
        let format = serde_json::to_value(suggestion.check.format).unwrap_or_default();
        println!(
            "  {} {} {}: {} format",
            suggestion.tool,
            suggestion.version,
            suggestion.port,
            format.as_str().unwrap_or_default()
        );
    }
    Ok(())
}
