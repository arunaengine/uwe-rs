//! Tool definitions in JSON through the public API.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::{Value as Json, json};
use tool_definition::{Definition, PortKind};

const BAM: &str = "format_2572";

fn tool(value: Json) -> Definition {
    serde_json::from_value(value).expect("a tool definition")
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
