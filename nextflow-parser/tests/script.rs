//! Tests how scripts parse into declarations and sections, and which script errors are reported.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_parser::ast::{
    ExpressionKind, Function, Item, Literal, Name, ParameterDeclaration, TypedName,
};
use nextflow_parser::parse_script;

/// Parses a script that must have no errors.
fn parse(source: &str) -> Vec<Item> {
    let parsed = parse_script(source);
    assert!(
        parsed.errors.is_empty(),
        "unexpected errors: {:?}",
        parsed.errors
    );
    parsed.tree.items
}

fn messages(source: &str) -> Vec<String> {
    let parsed = parse_script(source);
    parsed
        .errors
        .into_iter()
        .map(|error| error.message)
        .collect()
}

fn texts(names: &[Name]) -> Vec<&str> {
    names.iter().map(|name| name.text.as_str()).collect()
}

fn typed_names(names: &[TypedName]) -> Vec<&str> {
    names.iter().map(|typed| typed.name.text.as_str()).collect()
}

#[test]
fn feature_flags() {
    let items = parse("nextflow.enable.dsl = 2\nnextflow.preview.topic = true\n");
    assert_eq!(items.len(), 2);
    let Item::FeatureFlag { name, value, .. } = &items[0] else {
        panic!("expected a feature flag, found {:?}", items[0]);
    };
    assert_eq!(texts(name), ["enable", "dsl"]);
    let two = ExpressionKind::Literal(Literal::Integer("2".to_owned()));
    assert_eq!(value.kind, two);
}

#[test]
fn rejects_computed_flag() {
    assert_eq!(
        messages("nextflow.enable.types = !false\n"),
        ["Feature flag value must be a literal value (number, string, true/false)"]
    );
}

#[test]
fn include_names() {
    let items = parse(
        "include { FASTQC                 } from '../modules/nf-core/fastqc/main'
include { paramsSummaryMap       } from 'plugin/nf-schema'
include { SAMTOOLS_SORT as SORT; SAMTOOLS_INDEX } from './modules/samtools'
include {
    softwareVersionsToYAML
    methodsDescriptionText
} from '../subworkflows/nf-core/utils_nfcore_pipeline'
",
    );
    let includes: Vec<_> = items
        .iter()
        .map(|item| match item {
            Item::Include(include) => include,
            other => panic!("expected an include, found {other:?}"),
        })
        .collect();
    assert_eq!(includes.len(), 4);
    assert_eq!(includes[0].source.text, "../modules/nf-core/fastqc/main");
    assert_eq!(includes[1].source.text, "plugin/nf-schema");
    let sorted = &includes[2].names;
    assert_eq!(sorted[0].name.text, "SAMTOOLS_SORT");
    assert_eq!(
        sorted[0].alias.as_ref().map(|alias| &*alias.text),
        Some("SORT")
    );
    assert_eq!(sorted[1].name.text, "SAMTOOLS_INDEX");
    assert_eq!(sorted[1].alias, None);
    assert_eq!(includes[3].names.len(), 2);
}

#[test]
fn rejects_import() {
    let parsed = parse_script("import groovy.json.JsonSlurper\n");
    assert!(matches!(parsed.tree.items[..], [Item::Import { .. }]));
    let message =
        "Groovy `import` declarations are not supported -- use fully-qualified name inline instead";
    assert_eq!(parsed.errors[0].message, message);
}

#[test]
fn parameter_block() {
    let items = parse(
        "params {
    // Path to the samplesheet
    input: Path
    outdir: String = 'results'
    save_merged = false
}

workflow {
    main:
    println(params.outdir)
}
",
    );
    let Item::Parameters { parameters, .. } = &items[0] else {
        panic!("expected a params block, found {:?}", items[0]);
    };
    let shapes: Vec<_> = parameters
        .iter()
        .map(|declaration| match declaration {
            ParameterDeclaration::Parameter {
                name,
                annotation,
                default,
            } => (&*name.text, annotation.is_some(), default.is_some()),
            other => panic!("expected a parameter, found {other:?}"),
        })
        .collect();
    let expected = [
        ("input", true, false),
        ("outdir", true, true),
        ("save_merged", false, true),
    ];
    assert_eq!(shapes, expected);
}

#[test]
fn rejects_parameter_statement() {
    let source = "params {\n    input.path = 'x'\n}\n\nworkflow {\n}\n";
    assert_eq!(messages(source), ["Invalid parameter declaration"]);
}

#[test]
fn legacy_parameters() {
    let items = parse("params.outdir = 'results'\nparams.multiqc.title = null\n");
    let paths: Vec<_> = items
        .iter()
        .map(|item| match item {
            Item::LegacyParameter { path, .. } => texts(path),
            other => panic!("expected a legacy parameter, found {other:?}"),
        })
        .collect();
    assert_eq!(paths, [vec!["outdir"], vec!["multiqc", "title"]]);
}

#[test]
fn rejects_mixed_parameters() {
    let source = "params {\n    a = 1\n}\nparams.b = 2\n\nworkflow {\n}\n";
    assert_eq!(
        messages(source),
        ["Legacy parameter declarations cannot be mixed with the params block"]
    );
}

#[test]
fn record_fields() {
    let items = parse(
        "record Sample {
    id: String
    fastq_1: Path
    fastq_2: Path?
}
",
    );
    let Item::Record { name, fields, .. } = &items[0] else {
        panic!("expected a record, found {:?}", items[0]);
    };
    assert_eq!(name.text, "Sample");
    assert_eq!(typed_names(fields), ["id", "fastq_1", "fastq_2"]);
    assert!(fields.iter().all(|field| field.annotation.is_some()));
}

#[test]
fn rejects_incomplete_records() {
    assert_eq!(messages("record Sample {}\n"), ["Missing record body"]);
    assert_eq!(
        messages("record Sample {\n    id\n}\n"),
        ["Missing field type"]
    );
}

#[test]
fn enum_variants() {
    let items = parse("enum Strandedness {\n    FORWARD,\n    REVERSE,\n    UNSTRANDED,\n}\n");
    let Item::Enum { name, variants, .. } = &items[0] else {
        panic!("expected an enum, found {:?}", items[0]);
    };
    assert_eq!(name.text, "Strandedness");
    assert_eq!(texts(variants), ["FORWARD", "REVERSE", "UNSTRANDED"]);
}

#[test]
fn function_definitions() {
    let items = parse(
        "def greet(name) {
    return \"Hello ${name}!\"
}

def add(a: Integer, b: Integer) -> Integer {
    a + b
}

String shout(String text) {
    text.toUpperCase()
}

def getGenomeAttribute(attribute) {
    if (params.genomes && params.genome && params.genomes.containsKey(params.genome)) {
        return params.genomes[params.genome][attribute]
    }
    return null
}
",
    );
    let functions: Vec<&Function> = items
        .iter()
        .map(|item| match item {
            Item::Function(function) => &**function,
            other => panic!("expected a function, found {other:?}"),
        })
        .collect();
    let names: Vec<_> = functions
        .iter()
        .map(|function| &*function.name.text)
        .collect();
    assert_eq!(names, ["greet", "add", "shout", "getGenomeAttribute"]);
    assert_eq!(functions[0].parameters.len(), 1);
    assert_eq!(functions[0].return_type, None);
    assert_eq!(functions[1].parameters.len(), 2);
    assert!(functions[1].return_type.is_some());
    assert!(functions[2].return_type.is_some());
    assert_eq!(functions[3].body.statements.len(), 2);
}

#[test]
fn rejects_mixed_statements() {
    let source = "println 'Hello world!'\n\nworkflow {\n}\n";
    assert_eq!(
        messages(source),
        [
            "Statements cannot be mixed with script declarations -- move statements into a process, workflow, or function"
        ]
    );
    assert!(messages("println 'Hello world!'\nx = 1\n").is_empty());
}

#[test]
fn requires_entry_workflow() {
    assert_eq!(
        messages("params {\n    input: Path\n}\n"),
        ["Params block cannot be defined without an entry workflow"]
    );
    let twice = "workflow {\n}\n\nworkflow {\n}\n";
    assert_eq!(messages(twice), ["Entry workflow defined more than once"]);
}

#[test]
fn incomplete_declaration() {
    assert_eq!(messages("workflow.\n"), ["Incomplete declaration"]);
}

#[test]
fn recovers_after_error() {
    let parsed = parse_script(
        "include { } from './modules/broken'

process GOOD {
    script:
    \"echo good\"
}

enum Broken { A B }

workflow {
    GOOD()
}
",
    );
    assert_eq!(parsed.errors.len(), 2, "errors: {:?}", parsed.errors);
    let kinds: Vec<_> = parsed
        .tree
        .items
        .iter()
        .map(|item| match item {
            Item::Process(_) => "process",
            Item::Workflow(_) => "workflow",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["process", "workflow"]);
}
