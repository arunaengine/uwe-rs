//! Compiles small Argo workflows into wir and checks the graphs, dialect tasks and control.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::{Value as Json, json};

fn compiled(folder: &Path, spec: &str) -> (Json, Vec<argo_compiler::Kept>) {
    let target = folder.join("workflow.yaml");
    let text = format!(
        "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {{name: compiled}}\nspec:\n{spec}"
    );
    std::fs::write(&target, text).unwrap();
    let compiled = argo_compiler::compile(&target, &[], folder).unwrap();
    let json = serde_json::to_value(&compiled.document).unwrap();
    (json, compiled.kept)
}

fn graph<'d>(document: &'d Json, name: &str) -> &'d Json {
    let graphs = document["graphs"].as_array().unwrap();
    graphs
        .iter()
        .find(|graph| graph["name"] == name)
        .unwrap_or_else(|| panic!("graph {name}"))
}

fn op<'g>(graph: &'g Json, path: &str) -> &'g Json {
    let ops = graph["ops"].as_array().unwrap();
    ops.iter()
        .find(|op| op["path"] == path)
        .unwrap_or_else(|| panic!("op {path}"))
}

const LEAF: &str = r#"  entrypoint: say
  arguments: {parameters: [{name: text, value: hello}]}
  templates:
  - name: say
    inputs: {parameters: [{name: text}]}
    container: {image: alpine:3.20, command: [echo, "{{inputs.parameters.text}}"]}
    outputs: {parameters: [{name: out, valueFrom: {path: /tmp/out}}]}
"#;

#[test]
fn compiles_leaf_entry() {
    let folder = tempfile::tempdir().unwrap();
    let (document, kept) = compiled(folder.path(), LEAF);
    assert!(kept.is_empty(), "{kept:?}");
    let entry = graph(&document, "workflow");
    assert_eq!(
        entry["inputs"][0],
        json!({"name": "text", "type": "string", "default": "hello"})
    );
    let task = &op(entry, "main")["kind"]["task"]["builder"]["argo_task"];
    assert_eq!(task["entry"], "say");
    assert_eq!(task["call"], "entrypoint");
    let outputs: Vec<&Json> = (task["outputs"].as_array().unwrap().iter())
        .map(|map| &map["native"])
        .collect();
    let expected = [
        "parameter/outputs.parameters.out",
        "parameter/outputs.result",
        "parameter/exitCode",
    ];
    assert_eq!(outputs, expected.map(Json::from).iter().collect::<Vec<_>>());
    assert_eq!(entry["outputs"][0]["name"], "out");
    let container =
        r#"container: {image: alpine:3.20, command: [echo, "{{inputs.parameters.text}}"]}"#;
    let suspend = LEAF.replace(container, "suspend: {}");
    let (document, kept) = compiled(folder.path(), &suspend);
    assert!(kept[0].reason.contains("suspend"), "{kept:?}");
    let whole = &op(graph(&document, "workflow"), "workflow")["kind"]["native"];
    assert_eq!(whole["language"], "argo");
}
