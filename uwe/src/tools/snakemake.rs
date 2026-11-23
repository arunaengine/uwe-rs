//! Tool definitions of Snakemake rules: named inputs and outputs as ports with the EDAM formats
//! of their endings, params as parameters, and the container, threads and resources.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Map, Value as Json, json};
use snakemake_parser::ast::{Directive, Rule, Value};
use snakemake_parser::keyword::RuleKeyword;
use snakemake_parser::ruff_python_ast::Expr;
use snakemake_parser::ruff_text_size::Ranged;
use tool_definition::{
    Cardinality, Curation, Definition, Image, InputPort, OutputFormat, OutputPort, PortKind,
    Resources, SourceKind,
};

/// The definitions of the named rules of a Snakefile, one per rule.
pub(crate) fn definitions(path: &Path) -> Result<Vec<Definition>, String> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let file = snakemake_parser::parse(&text).map_err(|error| error.to_string())?;
    let source = super::source(SourceKind::Snakemake, path)?;
    let found: Vec<Definition> = (file.rules())
        .filter(|rule| rule.name.is_some())
        .map(|rule| definition(rule, &text, source.clone()))
        .collect();
    match found.is_empty() {
        true => Err("the Snakefile has no named rule".to_owned()),
        false => Ok(found),
    }
}

/// One argument of a directive: its name, or none for a positional one, and its source text.
struct Argument<'a> {
    name: Option<String>,
    text: &'a str,
}

fn arguments<'a>(directive: &Directive<RuleKeyword>, text: &'a str) -> Vec<Argument<'a>> {
    let Value::Arguments(arguments) = &directive.value else {
        return Vec::new();
    };
    let positional = (arguments.args.iter()).map(|expr| Argument {
        name: None,
        text: &text[expr.range()],
    });
    let named = (arguments.keywords.iter()).map(|keyword| Argument {
        name: keyword.arg.as_ref().map(|name| name.as_str().to_owned()),
        text: &text[keyword.value.range()],
    });
    positional.chain(named).collect()
}

fn definition(rule: &Rule, text: &str, source: tool_definition::Source) -> Definition {
    let name = rule
        .name
        .as_ref()
        .map(|name| name.text.clone())
        .unwrap_or_default();
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    let mut properties = Map::new();
    let mut image = None;
    let mut resources = Resources::default();
    for directive in &rule.directives {
        let found = arguments(directive, text);
        match directive.keyword {
            RuleKeyword::Input => {
                for (index, argument) in found.iter().enumerate() {
                    let (kind, cardinality, formats) = file(argument.text);
                    inputs.push(InputPort {
                        name: port_name(argument, "input", index),
                        title: None,
                        description: literal(argument.text).map(str::to_owned),
                        kind,
                        cardinality,
                        formats,
                        unit: None,
                        required: true,
                        default: None,
                        examples: Vec::new(),
                        conditions: Vec::new(),
                        active_when: None,
                    });
                }
            }
            RuleKeyword::Output => {
                let (named, loose): (Vec<_>, Vec<_>) =
                    found.iter().partition(|argument| argument.name.is_some());
                for argument in named {
                    let name = argument.name.clone().unwrap_or_default();
                    let description = literal(argument.text);
                    outputs.push(output_port(name, description, file(argument.text)));
                }
                // A Snakemake block gives the unnamed outputs as one list, `targets`.
                if let Some(first) = loose.first() {
                    let mut name = "targets".to_owned();
                    while found
                        .iter()
                        .any(|argument| argument.name.as_ref() == Some(&name))
                    {
                        name.push('_');
                    }
                    let (kind, _, _) = file(first.text);
                    let mut formats: Vec<String> = Vec::new();
                    for term in loose.iter().flat_map(|argument| file(argument.text).2) {
                        if !formats.contains(&term) {
                            formats.push(term);
                        }
                    }
                    let shape = (kind, Cardinality::List, formats);
                    outputs.push(output_port(name, None, shape));
                }
            }
            RuleKeyword::Parameters => {
                for (index, argument) in found.iter().enumerate() {
                    let mut schema = json!({});
                    if let Some(value) = literal(argument.text) {
                        schema = json!({"type": "string", "default": value});
                    } else if let Ok(number) = argument.text.trim().parse::<f64>() {
                        schema = json!({"type": "number", "default": number});
                    }
                    properties.insert(port_name(argument, "param", index), schema);
                }
            }
            RuleKeyword::Threads => {
                let first = found.first().map(|argument| argument.text.trim());
                resources.cpus = first.and_then(|text| text.parse().ok());
            }
            RuleKeyword::Resources => {
                for argument in &found {
                    let number = argument.text.trim().parse::<u64>().ok();
                    match argument.name.as_deref() {
                        Some("mem_mb") => resources.memory = number.map(|mb| mb << 20),
                        Some("runtime") => resources.time = number.map(|minutes| minutes * 60),
                        _ => {}
                    }
                }
            }
            RuleKeyword::Container | RuleKeyword::Singularity => {
                let named = found.first().and_then(|argument| literal(argument.text));
                image = named.map(|name| Image {
                    name: name.trim_start_matches("docker://").to_owned(),
                    digest: None,
                });
            }
            _ => {}
        }
    }
    let description = (rule.docstrings.iter())
        .filter_map(|expr| match expr {
            Expr::StringLiteral(literal) => Some(literal.value.to_str().trim().to_owned()),
            _ => None,
        })
        .next();
    let parameters = match properties.is_empty() {
        true => Json::Null,
        false => json!({"type": "object", "properties": properties}),
    };
    Definition {
        id: name.clone(),
        version: (image.as_ref())
            .and_then(|image| super::tag(&image.name))
            .unwrap_or_else(|| "1.0".to_owned()),
        title: name,
        description,
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
    }
}

/// An output port of a rule with the kind, cardinality and formats of its files.
fn output_port(
    name: String,
    description: Option<&str>,
    (kind, cardinality, formats): (PortKind, Cardinality, Vec<String>),
) -> OutputPort {
    OutputPort {
        name,
        title: None,
        description: description.map(str::to_owned),
        kind,
        cardinality,
        format: OutputFormat::Terms(formats),
        guarantees: Vec::new(),
        created_when: None,
        optional: false,
        schema: Json::Null,
    }
}

/// The name of a port: its keyword, else `<kind>_<n>` from 1, as Snakemake numbers them.
fn port_name(argument: &Argument, kind: &str, index: usize) -> String {
    (argument.name.clone()).unwrap_or_else(|| format!("{kind}_{}", index + 1))
}

/// The text of a plain or f-string literal, without its quotes.
fn literal(text: &str) -> Option<&str> {
    let text = text.trim();
    let text = text.strip_prefix(['f', 'r']).unwrap_or(text);
    let quote = text.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    text.strip_prefix(quote)?.strip_suffix(quote)
}

/// What a file argument is: a Directory for `directory(...)`, a list for `expand(...)`, with
/// the EDAM format of the ending of its pattern.
fn file(text: &str) -> (PortKind, Cardinality, Vec<String>) {
    let text = text.trim();
    let kind = match text.starts_with("directory(") {
        true => PortKind::Directory,
        false => PortKind::File,
    };
    let cardinality = match text.starts_with("expand(") || text.starts_with('[') {
        true => Cardinality::List,
        false => Cardinality::One,
    };
    // The pattern is the first literal inside the call, or the literal itself.
    let inner = (text.find(['"', '\'']))
        .map(|start| &text[start..])
        .and_then(|rest| {
            let quote = rest.chars().next()?;
            rest[1..].split(quote).next()
        })
        .unwrap_or_default();
    let formats = match kind {
        PortKind::File => tool_definition::formats::ending(inner)
            .map(str::to_owned)
            .into_iter()
            .collect(),
        _ => Vec::new(),
    };
    (kind, cardinality, formats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_rules() {
        let text = r#"
rule align:
    """Aligns reads."""
    input:
        reads="data/{sample}.fq.gz",
        reference="ref/genome.fa",
    output:
        bam="results/{sample}.bam",
        logs=directory("logs/{sample}"),
    params:
        extra="-M",
    threads: 4
    resources:
        mem_mb=2048,
    container: "docker://quay.io/biocontainers/bwa:0.7.18--he4a0461_1"
    shell: "bwa mem {params.extra} {input.reference} {input.reads} > {output.bam}"
"#;
        let file = snakemake_parser::parse(text).expect("a Snakefile");
        let rule = file.rules().next().expect("a rule");
        let source = tool_definition::Source {
            kind: SourceKind::Snakemake,
            address: "Snakefile".into(),
            revision: "sha256:00".into(),
        };
        let definition = definition(rule, text, source);
        assert_eq!(definition.id, "align");
        assert_eq!(definition.description.as_deref(), Some("Aligns reads."));
        assert_eq!(definition.inputs[0].formats, ["format_1930"]);
        assert_eq!(definition.inputs[1].formats, ["format_1929"]);
        assert_eq!(definition.outputs[1].kind, PortKind::Directory);
        assert_eq!(
            definition.parameters["properties"]["extra"]["default"],
            "-M"
        );
        assert_eq!(definition.resources.cpus, Some(4));
        assert_eq!(definition.resources.memory, Some(2048 << 20));
        let image = definition.image.expect("an image");
        assert_eq!(image.name, "quay.io/biocontainers/bwa:0.7.18--he4a0461_1");
        assert_eq!(definition.version, "0.7.18--he4a0461_1");
    }

    #[test]
    fn lists_unnamed_outputs() {
        let text = r#"
rule split:
    input: "data/{sample}.txt"
    output: "parts/a.txt", "parts/b.txt", targets="kept.txt"
    shell: "split {input}"
"#;
        let file = snakemake_parser::parse(text).expect("a Snakefile");
        let rule = file.rules().next().expect("a rule");
        let source = tool_definition::Source {
            kind: SourceKind::Snakemake,
            address: "Snakefile".into(),
            revision: "sha256:00".into(),
        };
        let definition = definition(rule, text, source);
        let ports: Vec<(&str, Cardinality)> = (definition.outputs.iter())
            .map(|port| (port.name.as_str(), port.cardinality))
            .collect();
        assert_eq!(
            ports,
            [
                ("targets", Cardinality::One),
                ("targets_", Cardinality::List)
            ],
            "as the block names them"
        );
    }
}
