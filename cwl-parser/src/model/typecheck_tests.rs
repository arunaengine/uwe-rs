//! Tests of the type check of workflow links, with the results of `cwltool --validate`
//! 3.3.20260925135507 for the same documents.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::Loader;
use crate::source::Memory;

const TOOL: &str = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: echo\n\
    inputs:\n  s: string?\n  l: long?\n  e: {type: ['null', {type: enum, symbols: [x, y]}]}\n  \
    f: File?\n  fs: File[]?\n  r: File\noutputs:\n  out: stdout\n";

/// Whether a workflow with these inputs, step inputs and step fields loads.
fn loads(inputs: &str, links: &str, step: &str) -> bool {
    let workflow = format!(
        "cwlVersion: v1.2\nclass: Workflow\nrequirements: {{ScatterFeatureRequirement: {{}}, \
         MultipleInputFeatureRequirement: {{}}}}\ninputs:\n  {inputs}\noutputs: []\nsteps:\n  \
         step:\n    run: tool.cwl\n    {step}\n    in:\n      \
         r: {{default: {{class: File, location: tool.cwl}}}}\n      {links}\n    out: [out]\n"
    );
    let mut memory = Memory::default();
    memory.insert("file:///work/tool.cwl", TOOL);
    memory.insert("file:///work/workflow.cwl", &workflow);
    Loader::new(memory)
        .load("file:///work/workflow.cwl")
        .is_ok()
}

#[test]
fn rejects_sources_of_other_types() {
    assert!(!loads("x: File", "s: x", ""));
    assert!(!loads("x: int", "l: x", ""));
    assert!(!loads(
        "x: {type: {type: enum, symbols: [a, b]}}",
        "s: x",
        ""
    ));
}

#[test]
fn accepts_enums_whatever_their_symbols_and_optional_sources() {
    assert!(loads(
        "x: {type: {type: enum, symbols: [a, b]}}",
        "e: x",
        ""
    ));
    assert!(loads("x: File?", "f: x", ""));
    assert!(loads("x: File", "f: x", ""));
}

#[test]
fn merges_only_sources_written_as_lists() {
    assert!(!loads(
        "x: File",
        "fs: {source: x, linkMerge: merge_nested}",
        ""
    ));
    assert!(loads(
        "x: File",
        "fs: {source: [x], linkMerge: merge_nested}",
        ""
    ));
}

#[test]
fn wraps_scattered_inputs_in_arrays() {
    assert!(loads("x: File[]", "f: x", "scatter: f"));
    assert!(!loads("x: File", "f: x", "scatter: f"));
}

#[test]
fn applies_pick_value_before_the_check() {
    let strings = "x: string\n  y: string";
    assert!(!loads(
        strings,
        "s: {source: [x, y], pickValue: all_non_null}",
        ""
    ));
    let files = "x: File?\n  y: File?";
    assert!(loads(
        files,
        "f: {source: [x, y], pickValue: first_non_null}",
        ""
    ));
}
