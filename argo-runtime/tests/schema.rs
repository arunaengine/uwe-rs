//! The inputs and outputs of Argo workflows as JSON Schema (decision 24).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::json;
use workflow_graph::Launch;

const WORKFLOW: &str = r#"apiVersion: argoproj.io/v1alpha1
kind: Workflow
metadata:
  generateName: greet-
  annotations:
    workflows.argoproj.io/description: Says hello.
spec:
  entrypoint: main
  arguments:
    parameters:
    - name: greeting
      value: hello
      enum: [hello, hi]
      description: What to say.
    - name: name
  templates:
  - name: main
    inputs:
      parameters:
      - name: greeting
    outputs:
      parameters:
      - name: said
        valueFrom: {path: /tmp/said}
        globalName: last-said
      artifacts:
      - name: log
        path: /tmp/log
        optional: true
    container:
      image: busybox
      command: [echo, "{{inputs.parameters.greeting}} {{workflow.parameters.name}}"]
"#;

#[test]
fn a_workflow_describes_its_parameters_and_outputs() {
    let folder = tempfile::tempdir().expect("a folder");
    let file = folder.path().join("greet.yaml");
    std::fs::write(&file, WORKFLOW).expect("writes the workflow");
    let launch = Launch {
        target: file,
        params: vec![("greeting".into(), "hi".into())],
        ..Launch::default()
    };
    let mut driver = argo_runtime::driver(&launch).expect("loads the workflow");
    let inputs = driver.inputs().expect("describes").expect("inputs");
    assert_eq!(inputs.schema["title"], "greet-");
    assert_eq!(inputs.schema["description"], "Says hello.");
    assert_eq!(inputs.schema["required"], json!(["name"]));
    assert_eq!(
        inputs.schema["properties"]["greeting"],
        json!({"type": "string", "enum": ["hello", "hi"], "default": "hello", "description": "What to say."})
    );
    assert_eq!(inputs.values["greeting"], "hi");
    assert_eq!(
        inputs.read,
        Some(["greeting".to_owned(), "name".to_owned()].into())
    );
    let problems = input_schema::check(&inputs, false);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(
        problems[0].message,
        "the input `name` is required, but has no value"
    );
    let outputs = driver.outputs().expect("describes").expect("outputs");
    assert_eq!(outputs["required"], json!(["said"]));
    assert_eq!(
        outputs["properties"]["log"],
        json!({"type": "string", "format": "path"})
    );
    assert_eq!(
        outputs["properties"]["last-said"]["description"],
        "a global output of the template main"
    );
    assert_eq!(input_schema::valid(&outputs), Ok(()));
}
