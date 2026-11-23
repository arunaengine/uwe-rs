//! Tool definitions of Snakemake rules, with ports, params, image and resources, and the
//! Snakefile that runs one rule on its own as a wir op.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Map, Value as Json, json};
use snakemake_parser::ast::{Directive, Rule, Statement, Value};
use snakemake_parser::keyword::{GlobalKeyword, RuleKeyword};
use snakemake_parser::ruff_python_ast::{Expr, Stmt};
use snakemake_parser::ruff_text_size::Ranged;
use tool_definition::{
    Cardinality, Curation, Definition, Image, InputPort, OutputFormat, OutputPort, PortKind,
    Resources, SourceKind,
};
use wir::Port;

use super::lowering::name;

/// The Python function of a wrapper that takes a wildcard from the name of an input file.
const WILDCARD: &str = "def wildcard_value(path, prefix, suffix):
    name = str(path).rsplit(\"/\", 1)[-1]
    if prefix and name.startswith(prefix):
        name = name[len(prefix):]
    if suffix and name.endswith(suffix):
        name = name[:len(name) - len(suffix)]
    return name
";

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
    let inner = pattern(text);
    let formats = match kind {
        PortKind::File => tool_definition::formats::ending(inner)
            .map(str::to_owned)
            .into_iter()
            .collect(),
        _ => Vec::new(),
    };
    (kind, cardinality, formats)
}

/// The pattern of a file argument: the first literal inside the call, or the literal itself.
fn pattern(text: &str) -> &str {
    (text.find(['"', '\'']))
        .map(|start| &text[start..])
        .and_then(|rest| {
            let quote = rest.chars().next()?;
            rest[1..].split(quote).next()
        })
        .unwrap_or_default()
}

/// The Snakefile that runs the rule of `definition` from `file` on its own, as one wir op: inputs
/// and params from the config, outputs without wildcards, and each wildcard the rule reads as a
/// param, by default from the name of the input file it comes from.
pub(crate) fn entry(
    file: &Path,
    definition: &Definition,
    ports: &[Port],
) -> Result<String, String> {
    let text = std::fs::read_to_string(file)
        .map_err(|error| format!("cannot read {}: {error}", file.display()))?;
    let parsed = snakemake_parser::parse(&text).map_err(|error| error.to_string())?;
    let rule = (parsed.rules())
        .find(|rule| (rule.name.as_ref()).is_some_and(|name| name.text == definition.id))
        .ok_or_else(|| format!("{} has no rule {}", file.display(), definition.id))?;
    let absolute = std::path::absolute(file).unwrap_or_else(|_| file.to_owned());
    let folder = absolute.parent().unwrap_or(Path::new("/"));
    let given = |port: &str| ports.iter().any(|known| known.name == name(port));
    let read = wildcards(rule, &text);
    let indent = (rule.directives.first())
        .map(|directive| line_indent(&text, directive.range.start().to_usize()))
        .unwrap_or("    ");
    let mut body: Vec<String> = Vec::new();
    let mut params_at = None;
    let mut used: Vec<String> = Vec::new();
    let mut inputs = Vec::new();
    for directive in &rule.directives {
        let found = arguments(directive, &text);
        match directive.keyword {
            RuleKeyword::Input => {
                inputs = found
                    .iter()
                    .map(|argument| (argument.name.clone(), argument.text))
                    .collect();
                let entries = (found.iter().enumerate()).map(|(index, argument)| {
                    let port = port_name(argument, "input", index);
                    let value = match given(&port) {
                        true => format!("config[{}]", quoted(&name(&port))),
                        false => fixed(argument.text, folder),
                    };
                    keyed(argument, value)
                });
                body.push(block(indent, "input", entries.collect()));
            }
            RuleKeyword::Output => {
                let entries = (found.iter().enumerate()).map(|(index, argument)| {
                    let trimmed = argument.text.trim();
                    if !trimmed.contains('{') || trimmed.starts_with("expand(") {
                        return keyed(argument, trimmed.to_owned());
                    }
                    let stem =
                        (argument.name.clone()).unwrap_or_else(|| format!("output_{}", index + 1));
                    let value = match trimmed.starts_with("directory(") {
                        true => format!("directory({})", quoted(&stem)),
                        false => quoted(&format!("{stem}{}", ending(pattern(trimmed)))),
                    };
                    keyed(argument, value)
                });
                body.push(block(indent, "output", entries.collect()));
                params_at.get_or_insert(body.len());
            }
            RuleKeyword::Parameters => {
                params_at = Some(body.len());
            }
            RuleKeyword::Log | RuleKeyword::Benchmark => {
                let keyword = directive.keyword.text();
                let entries = (found.iter().enumerate()).map(|(index, argument)| {
                    let trimmed = argument.text.trim();
                    if !trimmed.contains('{') {
                        return keyed(argument, trimmed.to_owned());
                    }
                    let stem = (argument.name.clone())
                        .unwrap_or_else(|| format!("{keyword}_{}", index + 1));
                    let fallback = match directive.keyword {
                        RuleKeyword::Log => ".log",
                        _ => ".tsv",
                    };
                    let kept = Some(ending(pattern(trimmed))).filter(|kept| !kept.is_empty());
                    keyed(
                        argument,
                        quoted(&format!("logs/{stem}{}", kept.unwrap_or(fallback))),
                    )
                });
                body.push(block(indent, keyword, entries.collect()));
            }
            RuleKeyword::Script | RuleKeyword::Notebook | RuleKeyword::Cwl | RuleKeyword::Conda => {
                let found = found.first().and_then(|argument| literal(argument.text));
                let place = found.filter(|path| !path.contains('{') && !path.contains("://"));
                let local = place
                    .map(|path| folder.join(path))
                    .filter(|path| path.exists());
                // These directives take one value, so it has no comma after it.
                match local {
                    Some(path) => body.push(format!(
                        "{indent}{}:\n{indent}    {}",
                        directive.keyword.text(),
                        quoted(&path.to_string_lossy())
                    )),
                    None => body.push(verbatim(&text, directive, &read, &mut used)),
                }
            }
            RuleKeyword::Name | RuleKeyword::WildcardConstraints => {}
            _ => body.push(verbatim(&text, directive, &read, &mut used)),
        }
    }
    // Positional params come before named ones, as in the call that Snakemake makes of them.
    let (mut positional, mut named) = (Vec::new(), Vec::new());
    for directive in (rule.directives.iter()).filter(|d| d.keyword == RuleKeyword::Parameters) {
        for (index, argument) in arguments(directive, &text).iter().enumerate() {
            let port = port_name(argument, "param", index);
            let original = replaced(argument.text.trim(), &read, &mut used);
            let value = format!("config.get({}, {original})", quoted(&name(&port)));
            match argument.name.is_some() {
                true => named.push(keyed(argument, value)),
                false => positional.push(value),
            }
        }
    }
    let mut helper = false;
    for wildcard in read.iter().filter(|wildcard| used.contains(wildcard)) {
        if named
            .iter()
            .any(|entry| entry.starts_with(&format!("{wildcard}=")))
        {
            continue;
        }
        let derived = (inputs.iter().enumerate()).find_map(|(index, (named, text))| {
            let port = named
                .clone()
                .unwrap_or_else(|| format!("input_{}", index + 1));
            let base = pattern(text).rsplit('/').next().unwrap_or_default();
            let (prefix, rest) = base.split_once(&format!("{{{wildcard}"))?;
            let suffix = &rest[rest.find('}')? + 1..];
            (given(&port) && !prefix.contains('{') && !suffix.contains('{')).then(|| {
                format!(
                    "wildcard_value(config[{}], {}, {})",
                    quoted(&name(&port)),
                    quoted(prefix),
                    quoted(suffix)
                )
            })
        });
        helper |= derived.is_some();
        let fallback = derived.unwrap_or_else(|| quoted("node"));
        named.push(format!(
            "{wildcard}=config.get({}, {fallback})",
            quoted(wildcard)
        ));
    }
    if !positional.is_empty() || !named.is_empty() {
        let at = params_at.unwrap_or(body.len()).min(body.len());
        positional.extend(named);
        body.insert(at, block(indent, "params", positional));
    }
    let mut header = format!(
        "# The rule {} as one wir op, made by `uwe lower`.\n",
        definition.id
    );
    for statement in &parsed.statements {
        if let Statement::Global(global) = statement
            && global.keyword == GlobalKeyword::ConfigFile
            && let Some(path) = arguments_text(global, &text).and_then(literal)
            && folder.join(path).is_file()
        {
            let path = folder.join(path).to_string_lossy().into_owned();
            header.push_str(&format!("configfile: {}\n", quoted(&path)));
        }
    }
    let rule_text = &text[rule.range];
    for statement in &parsed.python {
        if bound(statement).iter().any(|name| word_in(rule_text, name)) {
            header.push_str(&text[statement.range()]);
            header.push('\n');
        }
    }
    if helper {
        header.push_str(WILDCARD);
    }
    Ok(format!(
        "{header}\n\nrule {}:\n{}\n",
        definition.id,
        body.join("\n")
    ))
}

/// The names of the wildcards that the output patterns of a rule hold.
fn wildcards(rule: &Rule, text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for directive in (rule.directives.iter()).filter(|d| d.keyword == RuleKeyword::Output) {
        for argument in arguments(directive, text) {
            let mut rest = argument.text;
            while let Some(start) = rest.find('{') {
                let Some(end) = rest[start..].find('}') else {
                    break;
                };
                let inside = &rest[start + 1..start + end];
                let wildcard = inside.split(',').next().unwrap_or_default().trim();
                let plain = !wildcard.is_empty()
                    && wildcard
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_');
                if plain && !found.iter().any(|known| known == wildcard) {
                    found.push(wildcard.to_owned());
                }
                rest = &rest[start + end + 1..];
            }
        }
    }
    found
}

/// The text of a directive as written, with the wildcards that the wrapper takes as params read
/// from its params; `used` collects those it reads.
fn verbatim(
    text: &str,
    directive: &Directive<RuleKeyword>,
    wildcards: &[String],
    used: &mut Vec<String>,
) -> String {
    let start = directive.range.start().to_usize();
    let line = text[..start].rfind('\n').map_or(0, |at| at + 1);
    replaced(
        &text[line..directive.range.end().to_usize()],
        wildcards,
        used,
    )
}

/// A text with each `wildcards.<name>` of the given wildcards as `params.<name>`.
fn replaced(text: &str, wildcards: &[String], used: &mut Vec<String>) -> String {
    let mut text = text.to_owned();
    for wildcard in wildcards {
        let read = format!("wildcards.{wildcard}");
        let found: Vec<usize> = (text.match_indices(&read))
            .map(|(at, _)| at)
            .filter(|at| whole(&text, *at, read.len()))
            .collect();
        if !found.is_empty() && !used.contains(wildcard) {
            used.push(wildcard.clone());
        }
        for at in found.into_iter().rev() {
            text.replace_range(at..at + read.len(), &format!("params.{wildcard}"));
        }
    }
    text
}

/// The value of an input that nothing feeds: its file from the folder of the Snakefile when it
/// names one without wildcards, else no file.
fn fixed(text: &str, folder: &Path) -> String {
    let trimmed = text.trim();
    let plain = (trimmed.starts_with(['"', '\'']))
        .then(|| literal(trimmed))
        .flatten();
    match plain.filter(|path| !path.contains('{')) {
        Some(path) if path.contains("://") || Path::new(path).is_absolute() => quoted(path),
        Some(path) => quoted(&folder.join(path).to_string_lossy()),
        None => "[]".to_owned(),
    }
}

/// The ending of a file pattern after its last wildcard, like `.fq.gz` of `{sample}.fq.gz`.
fn ending(pattern: &str) -> &str {
    let base = pattern.rsplit('/').next().unwrap_or(pattern);
    let rest = base.rfind('}').map_or(base, |at| &base[at + 1..]);
    rest.find('.').map_or("", |at| &rest[at..])
}

/// A directive with one entry per line below its keyword.
fn block(indent: &str, keyword: &str, entries: Vec<String>) -> String {
    if entries.is_empty() {
        return format!("{indent}{keyword}: []");
    }
    let mut text = format!("{indent}{keyword}:");
    for entry in entries {
        text.push_str(&format!("\n{indent}    {entry},"));
    }
    text
}

/// An entry of a directive: `name=value` for a named argument, else the value alone.
fn keyed(argument: &Argument, value: String) -> String {
    match &argument.name {
        Some(name) => format!("{name}={value}"),
        None => value,
    }
}

/// The text of the one argument of a global directive, like the path of `configfile:`.
fn arguments_text<'a>(directive: &Directive<GlobalKeyword>, text: &'a str) -> Option<&'a str> {
    let Value::Arguments(arguments) = &directive.value else {
        return None;
    };
    arguments.args.first().map(|expr| &text[expr.range()])
}

/// The names that a top-level statement binds: imports, functions and assignments.
fn bound(statement: &Stmt) -> Vec<String> {
    match statement {
        Stmt::Import(import) => (import.names.iter())
            .map(|alias| {
                let shown = alias.asname.as_ref().unwrap_or(&alias.name).as_str();
                shown.split('.').next().unwrap_or(shown).to_owned()
            })
            .collect(),
        Stmt::ImportFrom(import) => (import.names.iter())
            .map(|alias| {
                alias
                    .asname
                    .as_ref()
                    .unwrap_or(&alias.name)
                    .as_str()
                    .to_owned()
            })
            .collect(),
        Stmt::FunctionDef(function) => vec![function.name.as_str().to_owned()],
        Stmt::Assign(assign) => (assign.targets.iter())
            .filter_map(|target| match target {
                Expr::Name(name) => Some(name.id.as_str().to_owned()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether `word` stands in `text` as a whole name, not as part of a longer one.
fn word_in(text: &str, word: &str) -> bool {
    (text.match_indices(word)).any(|(at, _)| whole(text, at, word.len()))
}

/// Whether the `length` bytes at `at` are a whole name of `text`, not part of a longer one or
/// an attribute of another value.
fn whole(text: &str, at: usize, length: usize) -> bool {
    let part = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let before = text[..at].chars().next_back();
    let after = text[at + length..].chars().next();
    !before.is_some_and(|c| part(c) || c == '.') && !after.is_some_and(part)
}

/// The indentation of the line that `offset` stands in.
fn line_indent(text: &str, offset: usize) -> &str {
    let line = text[..offset].rfind('\n').map_or(0, |at| at + 1);
    let rest = &text[line..];
    &rest[..rest.len() - rest.trim_start_matches([' ', '\t']).len()]
}

/// A text as a Python string literal.
fn quoted(text: &str) -> String {
    Json::String(text.to_owned()).to_string()
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

    const PIPELINE: &str = r#"import os

SAMPLES = ["a", "b"]

def label(name):
    return name.upper()

rule all:
    input: expand("counts/{sample}.txt", sample=SAMPLES)

rule count:
    input:
        reads="data/{sample}.fq.gz",
        reference="ref/genome.fa",
    output:
        counts="counts/{sample}.txt",
        logs=directory("logs/{sample}"),
    params:
        extra="-M",
        title=label("x"),
    log: "logs/{sample}.log"
    threads: 2
    container: "docker://debian:stable-slim"
    shell: "zcat {input.reads} | wc -l > {output.counts} # {wildcards.sample} {params.extra}"

rule plot:
    input: "counts/a.txt"
    output: "plot.svg"
    script: "scripts/plot.py"
"#;

    #[test]
    fn wraps_one_rule() {
        let folder = tempfile::tempdir().expect("a folder");
        let file = folder.path().join("Snakefile");
        std::fs::write(&file, PIPELINE).expect("writes the Snakefile");
        let found = definitions(&file).expect("definitions");
        let count = found
            .iter()
            .find(|tool| tool.id == "count")
            .expect("the rule");
        let port = |name: &str| Port {
            name: name.to_owned(),
            r#type: wir::Type::String,
            flow: Default::default(),
            default: None,
        };
        let text = entry(&file, count, &[port("reads"), port("extra")]).expect("a wrapper");
        let reference = folder.path().join("ref/genome.fa");
        for part in [
            "reads=config[\"reads\"],".to_owned(),
            format!("reference={},", quoted(&reference.to_string_lossy())),
            "counts=\"counts.txt\",".to_owned(),
            "logs=directory(\"logs\"),".to_owned(),
            "extra=config.get(\"extra\", \"-M\"),".to_owned(),
            "sample=config.get(\"sample\", wildcard_value(config[\"reads\"], \"\", \".fq.gz\")),"
                .to_owned(),
            "\"logs/log_1.log\",".to_owned(),
            "def label(name):".to_owned(),
            "# {params.sample} {params.extra}".to_owned(),
        ] {
            assert!(text.contains(&part), "{part}: {text}");
        }
        assert!(!text.contains("wildcards.sample"), "{text}");
        assert!(
            !text.contains("SAMPLES") && !text.contains("import os"),
            "{text}"
        );
        let parsed = snakemake_parser::parse(&text).expect("the wrapper parses");
        let names: Vec<&str> = (parsed.rules())
            .filter_map(|rule| rule.name.as_ref().map(|name| name.text.as_str()))
            .collect();
        assert_eq!(names, ["count"], "one rule, without its pipeline");
        std::fs::create_dir_all(folder.path().join("scripts")).expect("makes the folder");
        std::fs::write(folder.path().join("scripts/plot.py"), "").expect("writes the script");
        let plot = found
            .iter()
            .find(|tool| tool.id == "plot")
            .expect("the rule");
        let text = entry(&file, plot, &[port("input_1")]).expect("a wrapper");
        let script = folder.path().join("scripts/plot.py");
        let line = format!("script:\n        {}\n", quoted(&script.to_string_lossy()));
        assert!(
            text.ends_with(&line),
            "the script is found from anywhere: {text}"
        );
        snakemake_parser::parse(&text).expect("the wrapper parses");
    }
}
