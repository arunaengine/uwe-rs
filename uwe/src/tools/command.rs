//! The commands of `uwe tool`: the tools of a library, one tool, the tools of one file, the
//! check of a composed workflow, and the tools that fit a port or match some words.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tool_definition::{Definition, Fit, OutputFormat, Ranking, Suggestion, Supply};

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

/// `uwe tool extract <file>`: the tool definitions that uwe reads from one file.
pub fn extract(arguments: &Arguments) -> Result<(), Failure> {
    let path = (arguments.target.as_deref())
        .filter(|_| arguments.targets.is_empty())
        .ok_or_else(|| Failure::usage("uwe: use `uwe tool extract <file>`"))?;
    let found = super::extract(path).map_err(|reason| failure(format!("uwe: {reason}")))?;
    if arguments.json {
        let value = serde_json::to_value(&found).map_err(|error| failure(error.to_string()))?;
        return print_json(&value);
    }
    for tool in &found {
        println!("{}", described(tool, Some(path)));
    }
    Ok(())
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
