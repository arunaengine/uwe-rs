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
    error(inputs, links, step).is_none()
}

/// The error of a workflow with these inputs, step inputs and step fields, if any.
fn error(inputs: &str, links: &str, step: &str) -> Option<String> {
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
        .err()
        .map(|error| error.message)
}

#[test]
fn rejects_incompatible_sources() {
    assert!(!loads("x: File", "s: x", ""));
    assert!(!loads("x: int", "l: x", ""));
    assert!(!loads(
        "x: {type: {type: enum, symbols: [a, b]}}",
        "s: x",
        ""
    ));
}

#[test]
fn accepts_enum_sources() {
    assert!(loads(
        "x: {type: {type: enum, symbols: [a, b]}}",
        "e: x",
        ""
    ));
    assert!(loads("x: File?", "f: x", ""));
    assert!(loads("x: File", "f: x", ""));
}

#[test]
fn merges_list_sources() {
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
fn wraps_scatter_arrays() {
    assert!(loads("x: File[]", "f: x", "scatter: f"));
    assert!(!loads("x: File", "f: x", "scatter: f"));
}

#[test]
fn pick_precedes_typecheck() {
    let strings = "x: string\n  y: string";
    let message = error(strings, "s: {source: [x, y], pickValue: all_non_null}", "");
    assert!(
        message
            .as_ref()
            .is_some_and(|message| message
                .ends_with(" with pickValue all_non_null and linkMerge merge_nested")),
        "{message:?}"
    );
    let files = "x: File?\n  y: File?";
    assert!(loads(
        files,
        "f: {source: [x, y], pickValue: first_non_null}",
        ""
    ));
}
