//! Tests the resolve walk: identifier maps, both DSLs, identifier scopes, links and field names.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::Arc;

use crate::error::Error;
use crate::node::{Location, Node, Value};
use crate::salad::context::Namespaces;
use crate::salad::resolve::Resolver;
use crate::source::{Documents, Memory};

pub(crate) const URL: &str = "file:///work/flow.cwl";

/// Resolves the document at `URL`; the other files are available to imports.
pub(crate) fn resolve_files(files: &[(&str, &str)]) -> (Result<Node, Error>, Resolver) {
    let mut memory = Memory::default();
    for (url, text) in files {
        memory.insert(url, text);
    }
    let mut resolver = Resolver::new(Documents::new(Box::new(memory)));
    let at = Location::document(&Arc::from(URL));
    let resolved = resolver.document(URL, &at, &Namespaces::default());
    (resolved, resolver)
}

fn resolve(text: &str) -> Node {
    let (resolved, _) = resolve_files(&[(URL, text)]);
    resolved.unwrap_or_else(|error| panic!("{error}"))
}

/// A compact rendering of a node that ignores locations.
pub(crate) fn render(node: &Node) -> String {
    match &node.value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Integer(value) => value.to_string(),
        Value::Float(value) => value.to_string(),
        Value::String(text) => text.replace(URL, ""),
        Value::List(items) => {
            let items: Vec<String> = items.iter().map(render).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Map(entries) => {
            let entries: Vec<String> = entries
                .iter()
                .map(|entry| format!("{}: {}", entry.key.replace(URL, ""), render(&entry.value)))
                .collect();
            format!("{{{}}}", entries.join(", "))
        }
    }
}

fn field(node: &Node, key: &str) -> String {
    render(
        node.get(key)
            .unwrap_or_else(|| panic!("no `{key}` in {}", render(node))),
    )
}

#[test]
fn lists_identifier_maps() {
    let node = resolve("inputs:\n  b: File\n  a: {type: string, doc: first}\n");
    assert_eq!(
        field(&node, "inputs"),
        "[{type: string, doc: first, id: #a}, {type: File, id: #b}]"
    );
    let (failed, _) = resolve_files(&[(URL, "requirements:\n  DockerRequirement: alpine\n")]);
    let error = failed.expect_err("a requirement needs a map");
    assert_eq!(
        error.message,
        "the value of `DockerRequirement` must be a map"
    );
    assert_eq!((error.location.line, error.location.column), (2, 3));
}

#[test]
fn expands_type_dsl() {
    let node = resolve("inputs:\n  x: File[]?\n  y: [string?, int, 'null']\n  z: string[][]\n");
    assert_eq!(
        field(&node, "inputs"),
        "[{type: [null, {type: array, items: File}], id: #x}, \
         {type: [null, string, int], id: #y}, {type: string[][], id: #z}]"
    );
}

#[test]
fn expands_secondary_files() {
    let node = resolve("secondaryFiles: [.bai?, ^.dict, .bai?]\n");
    assert_eq!(
        field(&node, "secondaryFiles"),
        "[{pattern: .bai, required: false}, {pattern: ^.dict, required: null}]"
    );
}

#[test]
fn scopes_step_identifiers() {
    let text = "$graph:\n- id: main\n  class: Workflow\n  steps:\n    step1:\n      in: {a: x}\n      \
                out: [o]\n      run:\n        class: ExpressionTool\n        inputs: {a: int}\n";
    let node = resolve(text);
    let main = &node.get("$graph").and_then(Node::as_list).expect("a graph")[0];
    assert_eq!(field(main, "id"), "#main");
    let step = &main.get("steps").and_then(Node::as_list).expect("steps")[0];
    assert_eq!(field(step, "id"), "#main/step1");
    assert_eq!(field(step, "in"), "[{source: x, id: #main/step1/a}]");
    assert_eq!(field(step, "out"), "[#main/step1/o]");
    let run = step.get("run").expect("an inline process");
    assert_eq!(field(run, "inputs"), "[{type: int, id: #main/step1/run/a}]");
}

#[test]
fn resolves_link_fields() {
    let text = "cwlVersion: cwl:v1.0\nclass: cwl:CommandLineTool\n\
                requirements: [{class: cwl:DockerRequirement}]\nsteps: [{run: tools/cat.cwl}]\n";
    let node = resolve(text);
    assert_eq!(field(&node, "cwlVersion"), "v1.0");
    assert_eq!(field(&node, "class"), "CommandLineTool");
    assert_eq!(field(&node, "requirements"), "[{class: DockerRequirement}]");
    let step = &node.get("steps").and_then(Node::as_list).expect("steps")[0];
    assert_eq!(field(step, "run"), "file:///work/tools/cat.cwl");
}

#[test]
fn normalizes_field_names() {
    let text = "$namespaces: {s: 'http://schema.org/'}\nrdfs:label: tool\n\
                cwl:baseCommand: echo\ns:author: someone\n";
    let node = resolve(text);
    assert_eq!(field(&node, "label"), "tool");
    assert_eq!(field(&node, "baseCommand"), "echo");
    assert_eq!(field(&node, "http://schema.org/author"), "someone");
}

#[test]
fn injects_document_identifiers() {
    assert_eq!(field(&resolve("class: Workflow\n"), "id"), "");
    assert_eq!(field(&resolve("id: tool\n"), "id"), "#tool");
}
