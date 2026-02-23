//! Tests link checks: scoped references, named types, existing files and tolerated fields.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::error::Error;
use crate::links::check;
use crate::node::Node;
use crate::resolve::tests::{URL, render, resolve_files};

fn checked(files: &[(&str, &str)]) -> Result<Node, Error> {
    let (resolved, resolver) = resolve_files(files);
    let mut node = resolved?;
    check(&mut node, URL, &resolver.ids, &resolver.documents)?;
    Ok(node)
}

fn field(node: &Node, path: &[&str]) -> String {
    let mut current = node;
    for step in path {
        current = match step.parse::<usize>() {
            Ok(index) => &current.as_list().expect("a list")[index],
            Err(_) => current.get(step).unwrap_or_else(|| panic!("no `{step}`")),
        };
    }
    render(current)
}

const WORKFLOW: &str = "class: Workflow\ninputs: {x: File}\noutputs:\n  result:\n    type: File\n    \
                        outputSource: step1/o\nsteps:\n  step1:\n    in: {a: x}\n    out: [o]\n    \
                        run: tool.cwl\n    scatter: a\n";

#[test]
fn resolves_scoped_references() {
    let files = [
        (URL, WORKFLOW),
        ("file:///work/tool.cwl", "class: CommandLineTool\n"),
    ];
    let node = checked(&files).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(field(&node, &["outputs", "0", "outputSource"]), "#step1/o");
    assert_eq!(field(&node, &["steps", "0", "in", "0", "source"]), "#x");
    assert_eq!(field(&node, &["steps", "0", "scatter"]), "#step1/a");
}

#[test]
fn rejects_unknown_references() {
    let text = WORKFLOW.replace("in: {a: x}", "in: {a: y}");
    let files = [
        (URL, text.as_str()),
        ("file:///work/tool.cwl", "class: CommandLineTool\n"),
    ];
    let error = checked(&files).expect_err("`y` is not defined");
    assert_eq!(error.message, "`source` refers to unknown `y`");
    assert_eq!((error.location.line, error.location.column), (9, 13));
}

#[test]
fn resolves_named_types() {
    let text = "requirements:\n  - class: SchemaDefRequirement\n    types:\n      \
                - {name: Sample, type: record, fields: {path: File}}\n\
                inputs:\n  x: Sample[]\n  y: string\n";
    let node = checked(&[(URL, text)]).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        field(&node, &["inputs", "0", "type"]),
        "{type: array, items: #Sample}"
    );
    let unknown = text.replace("x: Sample[]", "x: Missing");
    let error = checked(&[(URL, unknown.as_str())]).expect_err("`Missing` is not defined");
    assert_eq!(error.message, "`type` refers to unknown `Missing`");
}

#[test]
fn checks_file_locations() {
    let listing = "requirements:\n  - class: InitialWorkDirRequirement\n    \
                   listing: [{class: File, location: data.txt}]\n";
    let error = checked(&[(URL, listing)]).expect_err("the file is missing");
    assert_eq!(
        error.message,
        "`location` refers to `file:///work/data.txt`, which does not exist"
    );
    let files = [(URL, listing), ("file:///work/data.txt", "content")];
    assert!(checked(&files).is_ok());
    let default = "inputs:\n  x:\n    type: File\n    default: {class: File, location: data.txt}\n";
    assert!(checked(&[(URL, default)]).is_ok());
}

#[test]
fn tolerates_hint_links() {
    assert!(checked(&[(URL, "hints: [{class: Unknown}]\n")]).is_ok());
    let error = checked(&[(URL, "requirements: [{class: Unknown}]\n")]).expect_err("unknown");
    assert_eq!(
        error.message,
        "`class` refers to `file:///work/Unknown`, which does not exist"
    );
}
