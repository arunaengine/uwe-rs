//! Tool definitions of Nextflow processes: path inputs and outputs as ports, values as
//! parameters, the container directive, and what the `meta.yml` of an nf-core module adds.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use nextflow_parser::ast::{Item, NamedOutput, Process, ProcessInput, TypedName};
use serde_json::{Map, Value as Json, json};
use tool_definition::edam::term_id;
use tool_definition::{
    Author, Citation, Curation, Definition, Image, InputPort, OutputFormat, OutputPort, PortKind,
    Resources, SourceKind, ValueType,
};

/// One declared input or output of a process: its qualifier, like `path` or `val`, and the
/// name or pattern it declares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Declared {
    pub qualifier: String,
    pub name: String,
}

/// An output of a process: its emit name, what it declares, and whether it is optional.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Emitted {
    pub emit: Option<String>,
    pub parts: Vec<Declared>,
    pub optional: bool,
}

/// The definitions of the processes of a Nextflow script, one per process.
pub(crate) fn definitions(path: &Path) -> Result<Vec<Definition>, String> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let parsed = nextflow_parser::parse_script(&text);
    if let Some(error) = parsed.errors.first() {
        return Err(format!("the script does not parse: {}", error.message));
    }
    let processes: Vec<&Process> = (parsed.tree.items.iter())
        .filter_map(|item| match item {
            Item::Process(process) => Some(process.as_ref()),
            _ => None,
        })
        .collect();
    if processes.is_empty() {
        return Err("the script has no process".to_owned());
    }
    let meta = path.parent().map(|folder| folder.join("meta.yml"));
    let meta = meta.filter(|file| file.is_file() && processes.len() == 1);
    let meta = match meta {
        Some(file) => {
            let read = std::fs::read_to_string(&file).map_err(|error| error.to_string())?;
            Some(tool_definition::yaml::read(&read).map_err(|error| format!("meta.yml: {error}"))?)
        }
        None => None,
    };
    let kind = match meta.is_some() {
        true => SourceKind::NfCore,
        false => SourceKind::Nextflow,
    };
    let source = super::source(kind, path)?;
    Ok((processes.iter())
        .map(|process| definition(process, &text, meta.as_ref(), source.clone()))
        .collect())
}

/// The inputs of a process, in order.
pub(crate) fn inputs(process: &Process, text: &str) -> Vec<Vec<Declared>> {
    (process.inputs.iter())
        .map(|input| match input {
            ProcessInput::Statement(statement) => declared(statement.span.text(text)),
            ProcessInput::Named(named) => vec![typed(named, text)],
            ProcessInput::Record(names) | ProcessInput::Tuple(names) => {
                names.iter().map(|named| typed(named, text)).collect()
            }
        })
        .collect()
}

/// The outputs of a process, in order.
pub(crate) fn outputs(process: &Process, text: &str) -> Vec<Emitted> {
    (process.outputs.iter())
        .map(|output| match output {
            NamedOutput::Statement(statement) => emitted(statement.span.text(text)),
            NamedOutput::Named { target, .. } => Emitted {
                emit: Some(target.name.text.clone()),
                parts: vec![typed(target, text)],
                optional: false,
            },
        })
        .collect()
}

fn definition(
    process: &Process,
    text: &str,
    meta: Option<&Json>,
    source: tool_definition::Source,
) -> Definition {
    let described = meta.map(described).unwrap_or_default();
    let mut ports = Vec::new();
    let mut properties = Map::new();
    for declared in inputs(process, text).into_iter().flatten() {
        let info = described.get(&declared.name);
        match declared.qualifier.as_str() {
            "path" | "file" => ports.push(InputPort {
                name: declared.name.clone(),
                title: None,
                description: info.and_then(|info| info.0.clone()),
                kind: PortKind::File,
                cardinality: Default::default(),
                formats: info.map(|info| info.1.clone()).unwrap_or_default(),
                unit: None,
                required: true,
                default: None,
                examples: Vec::new(),
                conditions: Vec::new(),
                active_when: None,
            }),
            // The meta map of nf-core modules is made from the sample, not given.
            "val" if declared.name == "meta" => {}
            "val" | "each" | "env" => {
                let mut schema = json!({"type": "string"});
                if let Some(text) = info.and_then(|info| info.0.clone()) {
                    schema["description"] = json!(text);
                }
                properties.insert(declared.name.clone(), schema);
            }
            _ => {}
        }
    }
    let mut results = Vec::new();
    for (index, output) in outputs(process, text).iter().enumerate() {
        let name = (output.emit.clone()).unwrap_or_else(|| format!("output_{}", index + 1));
        let info = described.get(&name);
        let file =
            (output.parts.iter()).find(|part| matches!(part.qualifier.as_str(), "path" | "file"));
        let kind = match file {
            Some(_) => PortKind::File,
            None => PortKind::Value(ValueType::String),
        };
        let mut formats: Vec<String> = info.map(|info| info.1.clone()).unwrap_or_default();
        if formats.is_empty()
            && let Some(term) = file.and_then(|part| tool_definition::formats::ending(&part.name))
        {
            formats.push(term.to_owned());
        }
        results.push(OutputPort {
            name,
            title: None,
            description: info.and_then(|info| info.0.clone()),
            kind,
            cardinality: Default::default(),
            format: OutputFormat::Terms(formats),
            guarantees: Vec::new(),
            created_when: None,
            optional: output.optional,
            schema: Json::Null,
        });
    }
    let directives: Vec<&str> = (process.directives.iter())
        .map(|statement| statement.span.text(text).trim())
        .collect();
    let image = (directives.iter())
        .find_map(|line| line.strip_prefix("container"))
        .and_then(container);
    let mut resources = Resources::default();
    for line in &directives {
        let mut words = line.split_whitespace();
        let (Some(name), Some(value)) = (words.next(), words.next()) else {
            continue;
        };
        let value = value.trim_matches(['\'', '"']);
        match name {
            "cpus" => resources.cpus = value.parse().ok(),
            "memory" => resources.memory = memory(line),
            _ => {}
        }
    }
    let name = process.name.text.clone();
    let meta_text = |key: &str| {
        meta.and_then(|meta| meta.get(key)?.as_str())
            .map(str::to_owned)
    };
    let words = |key: &str| -> Vec<String> {
        let items = meta.and_then(|meta| meta.get(key)?.as_array().cloned());
        let items = items.unwrap_or_default().into_iter();
        items
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect()
    };
    let tool = meta.and_then(first_tool);
    let version = (image.as_deref())
        .and_then(super::tag)
        .unwrap_or_else(|| "1.0".to_owned());
    let parameters = match properties.is_empty() {
        true => Json::Null,
        false => json!({"type": "object", "properties": properties}),
    };
    Definition {
        id: meta_text("name").unwrap_or_else(|| name.to_lowercase()),
        version,
        title: name,
        description: meta_text("description"),
        source: Some(source),
        image: image.map(|name| Image { name, digest: None }),
        license: (tool.as_ref()).and_then(|tool| first_text(tool.get("licence")?)),
        authors: (words("authors").into_iter())
            .map(|name| Author { name, orcid: None })
            .collect(),
        citations: (tool.as_ref())
            .and_then(|tool| tool.get("doi")?.as_str())
            .map(|doi| Citation {
                text: format!("doi:{doi}"),
                doi: Some(doi.to_owned()),
            })
            .into_iter()
            .collect(),
        biotools: (tool.as_ref())
            .and_then(|tool| tool.get("identifier")?.as_str())
            .and_then(|id| id.strip_prefix("biotools:"))
            .map(str::to_owned),
        operations: Vec::new(),
        topics: Vec::new(),
        keywords: words("keywords"),
        inputs: ports,
        parameters,
        outputs: results,
        resources,
        tests: Vec::new(),
        curation: Curation::Unchecked,
    }
}

/// What a typed input or output declares: a path when its type names one.
fn typed(named: &TypedName, text: &str) -> Declared {
    let annotation = named
        .annotation
        .as_ref()
        .map(|annotation| annotation.span.text(text));
    let path = annotation.is_some_and(|annotation| annotation.contains("Path"));
    Declared {
        qualifier: (if path { "path" } else { "val" }).to_owned(),
        name: named.name.text.clone(),
    }
}

/// The parts of a legacy declaration like `tuple val(meta), path(reads)` or `path fasta`.
pub(crate) fn declared(text: &str) -> Vec<Declared> {
    let text = text.trim();
    let text = text.strip_prefix("tuple").map_or(text, str::trim_start);
    let mut found = Vec::new();
    for part in split(text) {
        let part = part.trim();
        let qualifier: String = part
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        if qualifier.is_empty() || part[qualifier.len()..].trim_start().starts_with(':') {
            continue;
        }
        let rest = part[qualifier.len()..].trim_start();
        let inner = rest
            .strip_prefix('(')
            .and_then(|inner| inner.strip_suffix(')'));
        let first = split(inner.unwrap_or(rest))
            .into_iter()
            .next()
            .unwrap_or_default();
        let name = first.trim().trim_matches(['\'', '"']).to_owned();
        if !name.is_empty() {
            found.push(Declared { qualifier, name });
        }
    }
    found
}

fn emitted(text: &str) -> Emitted {
    let named = |key: &str| {
        (split(text).into_iter())
            .filter_map(|part| {
                part.trim()
                    .strip_prefix(key)?
                    .trim_start()
                    .strip_prefix(':')
                    .map(str::trim)
                    .map(str::to_owned)
            })
            .next()
    };
    // Only a plain identifier names an emit, never code that follows it.
    Emitted {
        emit: named("emit").filter(|name| identifier(name)),
        parts: declared(text),
        optional: named("optional").as_deref() == Some("true"),
    }
}

/// Whether a name is a Groovy identifier, as an emit name must be.
pub(crate) fn identifier(name: &str) -> bool {
    let mut chars = name.chars();
    (chars.next()).is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The parts of a text split at commas outside brackets and quotes.
fn split(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let (mut depth, mut quote, mut current) = (0i32, None, String::new());
    for c in text.chars() {
        match (quote, c) {
            (Some(open), _) if c == open => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, '(' | '[' | '{') => depth += 1,
            (None, ')' | ']' | '}') => depth -= 1,
            (None, ',') if depth == 0 => {
                parts.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    if !current.trim().is_empty() {
        parts.push(current);
    }
    parts
}

/// The Docker image of a `container` directive: the image after the `:` of the ternary that
/// nf-core modules use for Singularity, else the first quoted text.
fn container(rest: &str) -> Option<String> {
    let quoted = |quote: char| -> Vec<&str> { rest.split(quote).skip(1).step_by(2).collect() };
    let docker = match rest.contains('?') {
        true => quoted('\'')
            .into_iter()
            .rev()
            .find(|text| !text.contains("://")),
        false => (quoted('\'').into_iter().next()).or_else(|| quoted('"').into_iter().next()),
    };
    docker
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

/// The bytes of a `memory '2 GB'` directive.
fn memory(line: &str) -> Option<u64> {
    let value = line.split_once(char::is_whitespace)?.1;
    let value = value
        .trim()
        .trim_matches(['\'', '"'])
        .replace('.', " ")
        .to_uppercase();
    let mut words = value.split_whitespace();
    let number: u64 = words.next()?.parse().ok()?;
    let unit = match words.next().unwrap_or("B") {
        "B" => 1,
        "KB" => 1 << 10,
        "MB" => 1 << 20,
        "GB" => 1 << 30,
        "TB" => 1 << 40,
        _ => return None,
    };
    Some(number * unit)
}

/// The description and EDAM formats of each input and output that a `meta.yml` names, in the
/// old and the new layout of nf-core modules.
fn described(meta: &Json) -> std::collections::BTreeMap<String, (Option<String>, Vec<String>)> {
    let mut found = std::collections::BTreeMap::new();
    for key in ["input", "output"] {
        let Some(section) = meta.get(key) else {
            continue;
        };
        match section {
            // The new layout keeps outputs by emit name.
            Json::Object(emits) => {
                for (emit, value) in emits {
                    let (description, formats) = info(value);
                    found.insert(emit.clone(), (description, formats));
                }
            }
            other => collect(other, &mut found),
        }
    }
    found
}

/// The entries `{name: {type, description, ontologies}}` anywhere below a value.
fn collect(
    value: &Json,
    found: &mut std::collections::BTreeMap<String, (Option<String>, Vec<String>)>,
) {
    match value {
        Json::Array(items) => items.iter().for_each(|item| collect(item, found)),
        Json::Object(entries) => {
            for (name, entry) in entries {
                if entry.get("type").is_some() {
                    found.insert(name.clone(), info(entry));
                }
            }
        }
        _ => {}
    }
}

/// The description and the EDAM formats of the ontologies of an entry, or of the first file
/// entry below it.
fn info(value: &Json) -> (Option<String>, Vec<String>) {
    let mut description = None;
    let mut formats = Vec::new();
    walk(value, &mut |entry| {
        if entry.get("type").and_then(Json::as_str) == Some("map") {
            return;
        }
        if description.is_none() {
            description = entry
                .get("description")
                .and_then(Json::as_str)
                .map(str::to_owned);
        }
        for ontology in (entry.get("ontologies").and_then(Json::as_array))
            .into_iter()
            .flatten()
        {
            if let Some(iri) = ontology.get("edam").and_then(Json::as_str) {
                formats.push(term_id(iri).to_owned());
            }
        }
    });
    (description, formats)
}

fn walk(value: &Json, visit: &mut dyn FnMut(&Map<String, Json>)) {
    match value {
        Json::Array(items) => items.iter().for_each(|item| walk(item, visit)),
        Json::Object(entries) => {
            if entries.contains_key("type") {
                visit(entries);
                return;
            }
            entries.values().for_each(|item| walk(item, visit));
        }
        _ => {}
    }
}

/// The settings of the first tool that a `meta.yml` names, like its licence and DOI.
fn first_tool(meta: &Json) -> Option<Map<String, Json>> {
    let first = meta.get("tools")?.as_array()?.first()?.as_object()?;
    first.values().next()?.as_object().cloned()
}

fn first_text(value: &Json) -> Option<String> {
    match value {
        Json::String(text) => Some(text.clone()),
        Json::Array(items) => items.first()?.as_str().map(str::to_owned),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_declarations() {
        let found = declared("tuple val(meta), path(reads, stageAs: 'in/*')");
        let names: Vec<(&str, &str)> = (found.iter())
            .map(|part| (part.qualifier.as_str(), part.name.as_str()))
            .collect();
        assert_eq!(names, [("val", "meta"), ("path", "reads")]);
        let output = emitted(r#"tuple val(meta), path("*.html"), emit: html, optional: true"#);
        assert_eq!(output.emit.as_deref(), Some("html"));
        assert!(output.optional);
        let injected = emitted(r#"path("*.txt"), emit: out; System.exit(1)"#);
        assert_eq!(injected.emit, None, "code after an emit name is no name");
        assert_eq!(output.parts[1].name, "*.html");
        assert_eq!(declared("path fasta")[0].name, "fasta");
    }

    #[test]
    fn reads_containers() {
        let ternary = r#" "${ workflow.containerEngine == 'singularity' ? 'https://depot/fastqc:0.12.1--0' : 'biocontainers/fastqc:0.12.1--hdfd78af_0' }""#;
        assert_eq!(
            container(ternary).as_deref(),
            Some("biocontainers/fastqc:0.12.1--hdfd78af_0")
        );
        assert_eq!(
            container(" 'ubuntu:24.04'").as_deref(),
            Some("ubuntu:24.04")
        );
        assert_eq!(memory("memory '2 GB'"), Some(2 << 30));
        assert_eq!(memory("memory 2.GB"), Some(2 << 30));
    }
}
