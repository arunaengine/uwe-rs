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
const SEQUENCE: &str = r#"  entrypoint: main
  arguments: {parameters: [{name: word, value: hello}]}
  templates:
  - name: main
    inputs: {parameters: [{name: word}]}
    steps:
    - - {name: make, template: make, arguments: {parameters: [{name: word, value: "{{inputs.parameters.word}}"}]}}
    - - name: use
        template: use
        continueOn: {failed: true}
        arguments:
          parameters: [{name: text, value: "{{steps.make.outputs.parameters.text}}"}]
          artifacts: [{name: data, from: "{{steps.make.outputs.artifacts.data}}"}]
    outputs:
      parameters: [{name: text, valueFrom: {parameter: "{{steps.use.outputs.parameters.text}}"}}]
  - name: make
    inputs: {parameters: [{name: word}]}
    retryStrategy: {limit: 2}
    script: {image: alpine:3.20, command: [sh], source: "echo {{inputs.parameters.word}} > /tmp/text"}
    outputs:
      parameters: [{name: text, valueFrom: {path: /tmp/text}}]
      artifacts: [{name: data, path: /tmp/data}]
  - name: use
    inputs:
      parameters: [{name: text}]
      artifacts: [{name: data, path: /in/data, optional: true}]
    container: {image: alpine:3.20, command: [echo, "{{inputs.parameters.text}}"]}
    outputs:
      parameters: [{name: text, valueFrom: {path: /tmp/out}}]
"#;

#[test]
fn compiles_step_groups() {
    let folder = tempfile::tempdir().unwrap();
    let (document, kept) = compiled(folder.path(), SEQUENCE);
    assert!(kept.is_empty(), "{kept:?}");
    let entry = graph(&document, "workflow");
    assert_eq!(op(entry, "main")["kind"]["call"]["graph"], "template.main");
    assert_eq!(entry["outputs"][0]["name"], "text");
    let main = graph(&document, "template.main");
    let make = &op(main, "make")["kind"]["task"]["builder"]["argo_task"];
    assert_eq!(make["entry"], "main");
    assert_eq!(make["call"], "[0].make");
    let natives: Vec<&Json> = (make["inputs"].as_array().unwrap().iter())
        .map(|map| &map["native"])
        .collect();
    let expected = [
        json!("parameter/node.path"),
        json!("parameter/inputs.parameters.word"),
    ];
    assert_eq!(natives, expected.iter().collect::<Vec<_>>());
    let used = op(main, "use");
    assert_eq!(
        used["after"],
        json!([{"op": "make", "on": ["succeeded", "skipped"]}])
    );
    let ports: Vec<&Json> = (used["inputs"].as_array().unwrap().iter())
        .map(|port| &port["name"])
        .collect();
    assert!(ports.contains(&&json!("steps.make.outputs.artifacts.data")));
    let handler = op(main, "use.continue");
    assert_eq!(handler["handles"], json!([{"op": "use", "on": ["failed"]}]));
    let output = (main["edges"].as_array().unwrap().iter())
        .find(|edge| edge["to"]["port"] == "outputs.parameters.text");
    assert_eq!(
        output.unwrap()["from"],
        json!({"op": "use", "port": "outputs.parameters.text"})
    );
}

const DAG: &str = r#"  entrypoint: main
  templates:
  - name: main
    dag:
      tasks:
      - {name: a, template: say}
      - {name: b, template: say, depends: "a.Failed"}
      - {name: c, template: say, depends: "a || b"}
      - {name: lone, template: say}
      target: c
  - name: say
    container: {image: alpine:3.20, command: [echo, hi]}
"#;

#[test]
fn compiles_dag_control() {
    let folder = tempfile::tempdir().unwrap();
    let (document, kept) = compiled(folder.path(), DAG);
    assert!(kept.is_empty(), "{kept:?}");
    let main = graph(&document, "template.main");
    let paths: Vec<&Json> = main["ops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|op| &op["path"])
        .collect();
    assert!(
        !paths.contains(&&json!("lone")),
        "a task that the target does not need"
    );
    let b = op(main, "b");
    assert_eq!(
        b["depends"],
        json!({"result": {"op": "a", "on": ["failed"]}})
    );
    assert_eq!(b["handles"], json!([{"op": "a", "on": ["failed"]}]));
    let bare = json!(["succeeded", "skipped", "daemoned"]);
    let expected =
        json!({"any": [{"result": {"op": "a", "on": bare}}, {"result": {"op": "b", "on": bare}}]});
    assert_eq!(op(main, "c")["depends"], expected);
    assert!(op(main, "c").get("handles").is_none());
}

const LOOPS: &str = r#"  entrypoint: main
  templates:
  - name: main
    steps:
    - - {name: each, template: say, withItems: [a, {x: 1}], arguments: {parameters: [{name: text, value: "{{item}}"}]}}
      - {name: count, template: say, withSequence: {count: "2"}, arguments: {parameters: [{name: text, value: "{{item}}"}]}}
  - name: say
    inputs: {parameters: [{name: text}]}
    container: {image: alpine:3.20, command: [echo, "{{inputs.parameters.text}}"]}
"#;

#[test]
fn compiles_static_loops() {
    let folder = tempfile::tempdir().unwrap();
    let (document, kept) = compiled(folder.path(), LOOPS);
    assert!(kept.is_empty(), "{kept:?}");
    let main = graph(&document, "template.main");
    let each = op(main, "each");
    assert_eq!(
        each["kind"]["map"],
        json!({"body": "template.main.each", "over": ["item"]})
    );
    let item = |op: &Json| {
        op["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|port| port["name"] == "item")
            .cloned()
    };
    assert_eq!(item(each).unwrap()["default"], json!(["a", {"x": 1}]));
    assert_eq!(
        item(op(main, "count")).unwrap()["default"],
        json!(["0", "1"])
    );
    let body = graph(&document, "template.main.each");
    let task = &op(body, "each")["kind"]["task"]["builder"]["argo_task"];
    assert_eq!(task["call"], "[0].each");
    let maps = task["inputs"].as_array().unwrap();
    assert!(maps.iter().any(|map| map["native"] == "parameter/item"));
    let result =
        (each["outputs"].as_array().unwrap().iter()).find(|port| port["name"] == "outputs.result");
    assert_eq!(
        result.unwrap()["type"],
        json!({"array": {"items": {"optional": "string"}}})
    );
    let param = LOOPS.replace("withItems: [a, {x: 1}]", "withParam: '[\"a\"]'");
    let (_, kept) = compiled(folder.path(), &param);
    assert!(kept[0].reason.contains("withParam"), "{kept:?}");
}
