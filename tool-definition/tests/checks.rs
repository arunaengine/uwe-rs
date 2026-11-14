//! Tool definitions in JSON and checks of their connections through the public API.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use serde_json::{Value as Json, json};
use tool_definition::connection::check;
use tool_definition::{
    Aspect, Cardinality, Condition, Definition, Evidence, Fact, Fit, FormatMatch, InputPort,
    PortKind, Shape, SortKey, Supply,
};

const BAM: &str = "format_2572";
const SAM: &str = "format_2573";
const FASTA: &str = "format_1929";
const FASTQ: &str = "format_1930";

fn tool(value: Json) -> Definition {
    serde_json::from_value(value).expect("a tool definition")
}

fn port(value: Json) -> InputPort {
    serde_json::from_value(value).expect("an input port")
}

fn file(formats: &[&str], cardinality: Cardinality) -> Supply {
    Supply {
        kind: PortKind::File,
        shape: Shape {
            cardinality,
            per_sample: false,
        },
        formats: formats.iter().map(|term| (*term).to_owned()).collect(),
        facts: Vec::new(),
    }
}

/// samtools sort: any alignment in, BAM sorted by coordinate out.
fn sort() -> Definition {
    tool(json!({
        "id": "samtools-sort", "version": "1.21", "title": "Sort alignments",
        "operations": ["operation_3802"], "curation": "curated",
        "inputs": [{"name": "input", "kind": "file", "formats": [SAM, BAM]}],
        "outputs": [{"name": "bam", "kind": "file", "format": {"terms": [BAM]},
                     "guarantees": [{"sorted": "coordinate"}]}]
    }))
}

/// A variant caller that needs BAM sorted by coordinate.
fn caller() -> Definition {
    tool(json!({
        "id": "caller", "version": "2", "title": "Call variants", "curation": "imported",
        "inputs": [{"name": "alignments", "kind": "file", "formats": [BAM],
                    "conditions": [{"sorted": "coordinate"}]}],
        "outputs": [{"name": "vcf", "kind": "file", "format": {"terms": ["format_3016"]}}]
    }))
}

#[test]
fn bam_fits_either() {
    let found = check(
        &file(&[BAM], Cardinality::One),
        &port(json!({"name": "in", "kind": "file", "formats": [SAM, BAM]})),
    );
    assert_eq!(found.fit, Fit::Fits, "{found:?}");
    assert_eq!(found.format, FormatMatch::Exact);
    assert!(found.findings.is_empty(), "{found:?}");
}

#[test]
fn fasta_refuses_fastq() {
    let found = check(
        &file(&[FASTA], Cardinality::One),
        &port(json!({"name": "reads", "kind": "file", "formats": [FASTQ]})),
    );
    assert_eq!(found.fit, Fit::DoesNotFit, "{found:?}");
    assert_eq!(found.format, FormatMatch::Mismatch);
    assert_eq!(found.findings[0].aspect, Aspect::Format);
    assert!(
        found.findings[0].reason.contains("FASTA (format_1929)"),
        "{found:?}"
    );
}

#[test]
fn general_terms_answer() {
    let fastq_like = port(json!({"name": "reads", "kind": "file", "formats": ["format_2182"]}));
    let specific = check(&file(&[FASTQ], Cardinality::One), &fastq_like);
    assert_eq!(
        (specific.fit, specific.format),
        (Fit::Fits, FormatMatch::General)
    );

    let textual = check(
        &file(&["format_2330"], Cardinality::One),
        &port(json!({"name": "reads", "kind": "file", "formats": [FASTQ]})),
    );
    assert_eq!(
        (textual.fit, textual.format),
        (Fit::Unknown, FormatMatch::Unknown)
    );
    assert!(
        textual.findings[0].reason.contains("may still fit"),
        "{textual:?}"
    );
}

#[test]
fn unknown_term_warns() {
    let found = check(
        &file(&["format_99999"], Cardinality::One),
        &port(json!({"name": "in", "kind": "file", "formats": [BAM]})),
    );
    assert_eq!(found.fit, Fit::Unknown, "{found:?}");
    assert!(
        found.findings[0]
            .reason
            .contains("format_99999 is not a term of EDAM"),
        "{found:?}"
    );
    let untyped = check(
        &file(&[], Cardinality::One),
        &port(json!({"name": "in", "kind": "file", "formats": [BAM]})),
    );
    assert_eq!(untyped.fit, Fit::Unknown, "{untyped:?}");
}

#[test]
fn list_maps_samples() {
    let reads = port(json!({"name": "reads", "kind": "file", "formats": [FASTQ]}));
    let found = check(&file(&[FASTQ], Cardinality::List), &reads);
    assert_eq!(found.fit, Fit::Fits, "{found:?}");
    assert!(found.scatter);
    assert!(found.findings[0].reason.contains("sample id"), "{found:?}");

    let pair =
        port(json!({"name": "reads", "kind": "file", "cardinality": "pair", "formats": [FASTQ]}));
    let single = check(&file(&[FASTQ], Cardinality::One), &pair);
    assert_eq!(single.fit, Fit::DoesNotFit, "{single:?}");
}

#[test]
fn unsorted_bam_warns() {
    let alignments = &caller().inputs[0];
    let found = check(&file(&[BAM], Cardinality::One), alignments);
    assert_eq!(found.fit, Fit::Unknown, "{found:?}");
    assert_eq!(found.findings[0].aspect, Aspect::Condition);
    assert_eq!(found.findings[0].evidence, Some(Evidence::Unknown));

    let mut asserted = file(&[BAM], Cardinality::One);
    asserted.facts.push(Fact {
        condition: Condition::Sorted(SortKey::Coordinate),
        holds: false,
        evidence: Evidence::Asserted,
    });
    assert_eq!(check(&asserted, alignments).fit, Fit::DoesNotFit);
}

#[test]
fn sorted_output_fits() {
    let supply = Supply::output(&sort(), "bam", &BTreeMap::new(), false).expect("an output");
    let found = check(&supply, &caller().inputs[0]);
    assert_eq!(found.fit, Fit::Fits, "{found:?}");
    assert_eq!(found.findings[0].evidence, Some(Evidence::Guaranteed));
    assert!(found.findings[0].reason.contains("guarantees"), "{found:?}");
}

#[test]
fn definition_round_trip() {
    let value = json!({
        "id": "samtools-sort", "version": "1.21", "title": "Sort alignments",
        "source": {"kind": "nf_core", "address": "https://github.com/nf-core/modules",
                   "revision": "0123abc"},
        "image": {"name": "quay.io/biocontainers/samtools:1.21", "digest": "sha256:00ff"},
        "license": "MIT",
        "authors": [{"name": "A. Person", "orcid": "0000-0002-1825-0097"}],
        "citations": [{"text": "Twelve years of SAMtools", "doi": "10.1093/gigascience/giab008"}],
        "biotools": "samtools",
        "operations": ["operation_3802"], "topics": ["topic_0102"],
        "inputs": [{"name": "input", "title": "Alignments", "kind": "file",
                    "cardinality": "list", "formats": [BAM], "required": false,
                    "examples": ["a.bam"], "conditions": ["indexed", {"paired_with": "reads"}]},
                   {"name": "threads", "kind": {"value": "integer"}, "cardinality": "one",
                    "unit": "threads",
                    "required": true, "default": 1}],
        "outputs": [{"name": "bam", "kind": "file", "cardinality": "one",
                     "format": {"follows": "input"},
                     "guarantees": [{"sorted": "coordinate"}, {"other": "deduplicated"}]}],
        "resources": {"cpus": 2, "memory": 1073741824, "time": 600},
        "tests": [{"name": "small", "inputs": {"input": {"file": "tests/a.bam"},
                                               "threads": {"value": 1}},
                   "outputs": {"bam": {"contains": ["@HD"]}},
                   "results": {"slurm": {"passed": true, "at": "2026-11-14T12:00:00Z"}}}],
        "curation": "imported"
    });
    let definition = tool(value.clone());
    assert_eq!(
        definition.inputs[1].kind,
        PortKind::Value(tool_definition::ValueType::Integer)
    );
    assert_eq!(serde_json::to_value(&definition).expect("json"), value);
    let unknown = serde_json::from_value::<Definition>(json!({
        "id": "x", "version": "1", "title": "X", "extra": true
    }));
    assert!(unknown.is_err(), "unknown fields are refused");
}
