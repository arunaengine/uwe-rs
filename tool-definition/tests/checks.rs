//! Checks of connections, graphs and suggestions through the public API, with JSON tools.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use serde_json::{Value as Json, json};
use tool_definition::connection::check;
use tool_definition::suggest::{followers, producers};
use tool_definition::{
    Aspect, Cardinality, Condition, Definition, Edge, End, Evidence, Fact, Fit, FormatMatch, Graph,
    InputPort, Node, PortKind, Ranking, Shape, SortKey, Supply, graph,
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

/// A filter whose output keeps the format of its input, as Galaxy's `format_source`.
fn filter() -> Definition {
    tool(json!({
        "id": "filter", "version": "1", "title": "Filter alignments",
        "inputs": [{"name": "input", "kind": "file", "formats": [SAM, BAM]}],
        "parameters": {"type": "object", "properties": {"quality": {"type": "integer"}},
                       "required": ["quality"]},
        "outputs": [{"name": "kept", "kind": "file", "format": {"follows": "input"}}]
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
fn follows_input_format() {
    let node = |id: &str, tool: &Definition, given: BTreeMap<String, Supply>| Node {
        id: id.to_owned(),
        tool: tool.id.clone(),
        version: tool.version.clone(),
        parameters: json!({"quality": 20}),
        given,
    };
    let edge = |from: &str, output: &str, to: &str, input: &str| Edge {
        from: End {
            node: from.to_owned(),
            port: output.to_owned(),
        },
        to: End {
            node: to.to_owned(),
            port: input.to_owned(),
        },
        facts: Vec::new(),
    };
    let tools = [filter(), sort(), caller()];
    for (given, expected) in [(SAM, Fit::DoesNotFit), (BAM, Fit::Unknown)] {
        let input = BTreeMap::from([("input".to_owned(), file(&[given], Cardinality::One))]);
        let graph = Graph {
            nodes: vec![
                node("call", &tools[2], BTreeMap::new()),
                node("filter", &tools[0], input),
            ],
            edges: vec![edge("filter", "kept", "call", "alignments")],
        };
        let report = graph::check(&graph, &tools);
        assert_eq!(report.edges[0].fit, expected, "{given}: {report:?}");
        assert_eq!(report.nodes[1].fit, Fit::Fits, "{report:?}");
    }
}

#[test]
fn graph_reports_nodes() {
    let tools = [filter(), sort(), caller()];
    let samples = file(&[SAM], Cardinality::List);
    let graph: Graph = serde_json::from_value(json!({
        "nodes": [
            {"id": "sort", "tool": "samtools-sort", "version": "1.21",
             "given": {"input": serde_json::to_value(&samples).expect("json")}},
            {"id": "call", "tool": "caller", "version": "2"},
            {"id": "filter", "tool": "filter", "version": "1", "parameters": {}},
            {"id": "lost", "tool": "missing", "version": "0"}
        ],
        "edges": [
            {"from": {"node": "sort", "port": "bam"}, "to": {"node": "call", "port": "alignments"}},
            {"from": {"node": "sort", "port": "nothing"}, "to": {"node": "filter", "port": "input"}}
        ]
    }))
    .expect("a graph");
    let report = graph::check(&graph, &tools);
    assert_eq!(report.fit, Fit::DoesNotFit);
    let [sorted, call, filter, lost] = report.nodes.as_slice() else {
        panic!("four nodes: {report:?}");
    };
    assert!(sorted.scatter && sorted.fit == Fit::Fits, "{sorted:?}");
    assert!(
        call.scatter,
        "per-sample BAM files map the caller too: {call:?}"
    );
    assert_eq!(report.edges[0].fit, Fit::Fits, "{report:?}");
    let reasons: Vec<&str> = filter
        .findings
        .iter()
        .map(|finding| finding.reason.as_str())
        .collect();
    assert_eq!(filter.findings.len(), 2, "{reasons:?}");
    assert!(
        reasons.iter().any(|reason| reason.contains("quality")),
        "{reasons:?}"
    );
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("input input is not connected")),
        "{reasons:?}"
    );
    assert!(
        lost.findings[0]
            .reason
            .contains("missing version 0 is not known"),
        "{lost:?}"
    );
    assert_eq!(report.edges[1].fit, Fit::DoesNotFit, "{report:?}");
}

#[test]
fn absent_parameters_empty() {
    let schema = |required: Json| {
        json!({"type": "object", "properties": {"level": {"type": "integer"}},
               "required": required})
    };
    let optional = tool(json!({
        "id": "optional", "version": "1", "title": "Optional", "parameters": schema(json!([]))
    }));
    let required = tool(json!({
        "id": "required", "version": "1", "title": "Required",
        "parameters": schema(json!(["level"]))
    }));
    let graph: Graph = serde_json::from_value(json!({
        "nodes": [{"id": "a", "tool": "optional", "version": "1"},
                  {"id": "b", "tool": "required", "version": "1"}],
        "edges": []
    }))
    .expect("a graph");
    let report = graph::check(&graph, &[optional, required]);
    assert_eq!(report.nodes[0].fit, Fit::Fits, "{report:?}");
    let findings = &report.nodes[1].findings;
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert_eq!(findings[0].aspect, Aspect::Parameter);
    assert!(findings[0].reason.contains("level"), "{findings:?}");
}

#[test]
fn suggestions_rank() {
    let view = tool(json!({
        "id": "view", "version": "1", "title": "View anything", "curation": "curated",
        "inputs": [{"name": "any", "kind": "file"}]
    }));
    let text = tool(json!({
        "id": "text", "version": "1", "title": "Text reader",
        "inputs": [{"name": "text", "kind": "file", "formats": ["format_2330"]}]
    }));
    let reads = tool(json!({
        "id": "reads", "version": "1", "title": "Reads only",
        "inputs": [{"name": "reads", "kind": "file", "formats": [FASTQ]}]
    }));
    let tools = [view, text, caller(), filter(), sort(), reads];
    let found = followers(&file(&[BAM], Cardinality::One), &tools, &Ranking::default());
    let order: Vec<(&str, FormatMatch)> = found
        .iter()
        .map(|found| (found.tool.as_str(), found.check.format))
        .collect();
    assert_eq!(
        order,
        [
            ("samtools-sort", FormatMatch::Exact),
            ("caller", FormatMatch::Exact),
            ("filter", FormatMatch::Exact),
            ("view", FormatMatch::General),
        ],
        "curated before imported before unchecked; text and reads do not fit"
    );

    let ranking = Ranking {
        operations: Vec::new(),
        uses: BTreeMap::from([("filter".to_owned(), 9)]),
    };
    let wanted = port(json!({"name": "in", "kind": "file", "formats": [BAM]}));
    let found = producers(&wanted, &tools, &ranking);
    let order: Vec<(&str, FormatMatch)> = found
        .iter()
        .map(|found| (found.tool.as_str(), found.check.format))
        .collect();
    assert_eq!(
        order,
        [
            ("samtools-sort", FormatMatch::Exact),
            ("filter", FormatMatch::Unknown)
        ],
        "the filter's output follows an input that takes SAM or BAM"
    );
}

#[test]
fn operations_rank() {
    let sort = sort();
    let mut other = caller();
    other.curation = sort.curation;
    let ranking = Ranking {
        operations: vec!["operation_2409".to_owned()],
        uses: BTreeMap::new(),
    };
    let found = followers(&file(&[BAM], Cardinality::One), &[other, sort], &ranking);
    let order: Vec<&str> = found.iter().map(|found| found.tool.as_str()).collect();
    assert_eq!(
        order,
        ["samtools-sort", "caller"],
        "sorting is a kind of data handling"
    );
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

/// A tool whose reads input is used only in its paired branch, as in a Galaxy conditional.
fn branched() -> Definition {
    tool(json!({
        "id": "branched", "version": "1", "title": "Branched",
        "inputs": [{"name": "mode|mates", "kind": "file",
                    "active_when": {"properties": {"mode": {"properties": {
                        "layout": {"const": "paired"}}, "required": ["layout"]}},
                                    "required": ["mode"]}}],
        "parameters": {"type": "object"},
        "outputs": [{"name": "log", "kind": "file", "created_when": "keep['log'] is True"}]
    }))
}

#[test]
fn branch_input_required() {
    let graph: Graph = serde_json::from_value(json!({
        "nodes": [
            {"id": "single", "tool": "branched", "version": "1",
             "parameters": {"mode": {"layout": "single"}}},
            {"id": "paired", "tool": "branched", "version": "1",
             "parameters": {"mode": {"layout": "paired"}}},
            {"id": "unset", "tool": "branched", "version": "1"}
        ],
        "edges": []
    }))
    .expect("a graph");
    let report = graph::check(&graph, &[branched()]);
    let [single, paired, unset] = report.nodes.as_slice() else {
        panic!("three nodes: {report:?}");
    };
    assert_eq!(
        single.fit,
        Fit::Fits,
        "the other branch needs no mates: {single:?}"
    );
    assert_eq!(paired.fit, Fit::DoesNotFit, "{paired:?}");
    assert!(
        paired.findings[0].reason.contains("mode|mates"),
        "{paired:?}"
    );
    assert_eq!(unset.fit, Fit::Unknown, "no parameters yet: {unset:?}");
    assert_eq!(unset.findings[0].aspect, Aspect::Connection, "{unset:?}");
    let definition = branched();
    assert_eq!(
        definition.outputs[0].created_when.as_deref(),
        Some("keep['log'] is True")
    );
    assert!(definition.inputs[0].active_when.is_some());
}

#[test]
fn chosen_case_input() {
    let case = |layout: &str, default: bool| {
        let mut condition = json!({"properties": {"mode": {"properties": {
            "layout": {"const": layout}}, "required": ["layout"]}}, "required": ["mode"]});
        if default {
            condition =
                json!({"properties": {"mode": {"properties": {"layout": {"const": layout}}}}});
        }
        condition
    };
    let tool = tool(json!({
        "id": "cases", "version": "1", "title": "Cases",
        "inputs": [{"name": "mode|reads", "kind": "file", "formats": [FASTQ],
                    "active_when": case("fastq", true)},
                   {"name": "mode|reads", "kind": "file", "formats": [FASTA],
                    "active_when": case("fasta", false)}],
        "parameters": {"type": "object"}
    }));
    let supply = serde_json::to_value(file(&[FASTA], Cardinality::One)).expect("json");
    let graph: Graph = serde_json::from_value(json!({
        "nodes": [{"id": "n", "tool": "cases", "version": "1",
                   "parameters": {"mode": {"layout": "fasta"}},
                   "given": {"mode|reads": supply}}],
        "edges": []
    }))
    .expect("a graph");
    let report = graph::check(&graph, &[tool]);
    assert_eq!(
        report.nodes[0].fit,
        Fit::Fits,
        "the fasta case takes FASTA: {report:?}"
    );
}
