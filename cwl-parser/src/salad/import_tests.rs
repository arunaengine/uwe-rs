//! Tests `$import` and `$include`: splicing, flattening, fragments, cycles and errors.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::salad::resolve::tests::{URL, render, resolve_files};

fn resolved(files: &[(&str, &str)]) -> String {
    let (resolved, _) = resolve_files(files);
    render(&resolved.unwrap_or_else(|error| panic!("{error}")))
}

fn failed(files: &[(&str, &str)]) -> String {
    let (resolved, _) = resolve_files(files);
    match resolved {
        Ok(node) => panic!("resolved as {}", render(&node)),
        Err(error) => format!(
            "{}:{}: {}",
            error.location.line, error.location.column, error.message
        ),
    }
}

#[test]
fn imports_documents() {
    let files = [
        (URL, "requirements:\n  - $import: env.yml\n"),
        (
            "file:///work/env.yml",
            "class: EnvVarRequirement\nenvDef: {A: b}\n",
        ),
    ];
    assert_eq!(
        resolved(&files),
        "{requirements: [{class: EnvVarRequirement, envDef: [{envValue: b, envName: A}], \
         id: file:///work/env.yml}], id: }"
    );
}

#[test]
fn flattens_imported_lists() {
    let files = [
        (
            URL,
            "types:\n  - $import: types.yml\n  - {name: C, type: enum, symbols: [x]}\n",
        ),
        (
            "file:///work/types.yml",
            "- {name: A, type: record}\n- {name: B, type: record}\n",
        ),
    ];
    assert_eq!(
        resolved(&files),
        "{types: [{name: file:///work/types.yml#A, type: record}, \
         {name: file:///work/types.yml#B, type: record}, \
         {name: #C, type: enum, symbols: [#C/x]}], id: }"
    );
}

#[test]
fn includes_text() {
    let files = [
        (URL, "listing:\n  - entry: {$include: run.sh}\n"),
        ("file:///work/run.sh", "echo hello\n"),
    ];
    assert_eq!(resolved(&files), "{listing: [{entry: echo hello\n}], id: }");
}

#[test]
fn imports_fragments() {
    let files = [
        (URL, "types:\n  - $import: types.yml#B\n"),
        (
            "file:///work/types.yml",
            "- {name: A, type: record}\n- {name: B, type: enum}\n",
        ),
    ];
    assert_eq!(
        resolved(&files),
        "{types: [{name: file:///work/types.yml#B, type: enum}], id: }"
    );
}

#[test]
fn rejects_extra_fields() {
    let files = [(URL, "doc: {$include: a.txt, extra: 1}\n")];
    assert_eq!(
        failed(&files),
        "1:7: `$include` must be the only field of its map"
    );
}

#[test]
fn rejects_import_cycles() {
    let files = [
        (URL, "a: {$import: other.yml}\n"),
        ("file:///work/other.yml", "b: {$import: flow.cwl}\n"),
    ];
    assert_eq!(
        failed(&files),
        "1:14: `file:///work/flow.cwl` imports itself"
    );
}

#[test]
fn reports_missing_files() {
    let files = [(
        URL,
        "inputs:\n  script:\n    default: {$include: missing.py}\n",
    )];
    assert_eq!(
        failed(&files),
        "3:25: cannot read `file:///work/missing.py`: no such document"
    );
}
