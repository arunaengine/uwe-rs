//! Native Argo output and HTTP verdicts drive per-attempt acceptance.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use workflow_graph::{Driver, Launch, Outcome, OutputFile, Task};

fn driver(folder: &Path, spec: &str) -> Box<dyn Driver> {
    let target = folder.join("workflow.yaml");
    std::fs::write(
        &target,
        format!(
            "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {{name: acceptance}}\nspec:\n{spec}"
        ),
    )
    .unwrap();
    argo_runtime::driver(&Launch {
        target,
        ..Launch::default()
    })
    .unwrap()
}

fn output(folder: &Path, task: &Task, attempt: u32, paths: &[(&str, &str)]) -> Outcome {
    let files = paths
        .iter()
        .map(|(path, text)| {
            let location = folder.join(path.trim_start_matches('/').replace('/', "-"));
            std::fs::write(&location, text).unwrap();
            OutputFile {
                path: (*path).into(),
                location: location.to_string_lossy().into_owned(),
                ..OutputFile::default()
            }
        })
        .collect();
    Outcome {
        id: task.id,
        attempt,
        files,
        ..Outcome::default()
    }
}

const OUTPUTS: &str = r#"  entrypoint: main
  templates:
  - name: main
    steps:
    - - name: make
        template: make
        continueOn: {error: true}
    - - {name: after, template: after}
  - name: make
    container: {image: busybox}
    outputs:
      parameters: [{name: value, valueFrom: {path: /value}}]
      artifacts: [{name: file, path: /file}]
  - name: after
    container: {image: busybox}
"#;

#[test]
fn outputs_gate_acceptance() {
    for (paths, accepted) in [
        (Vec::new(), false),
        (vec![("/value", "ok"), ("/file", "bytes")], true),
    ] {
        let folder = tempfile::tempdir().unwrap();
        let mut driver = driver(folder.path(), OUTPUTS);
        let task = driver.start().unwrap().remove(0);
        let next = driver
            .complete(output(folder.path(), &task, 1, &paths))
            .unwrap();
        assert_eq!(next.len(), 1, "continueOn starts the next step");
        assert!(!driver.take_acceptance(task.id, 2));
        assert_eq!(driver.take_acceptance(task.id, 1), accepted);
        assert!(!driver.take_acceptance(task.id, 1));
    }
}

#[test]
fn separates_retry_verdicts() {
    let folder = tempfile::tempdir().unwrap();
    let spec = r#"  entrypoint: make
  templates:
  - name: make
    retryStrategy: {limit: 1, retryPolicy: OnError}
    container: {image: busybox}
    outputs: {artifacts: [{name: file, path: /file}]}
"#;
    let mut driver = driver(folder.path(), spec);
    let first = driver.start().unwrap().remove(0);
    let second = driver
        .complete(output(folder.path(), &first, 1, &[]))
        .unwrap()
        .remove(0);
    assert!(!driver.take_acceptance(first.id, 1));
    driver
        .complete(output(folder.path(), &second, 2, &[("/file", "ok")]))
        .unwrap();
    assert!(!driver.take_acceptance(second.id, 1));
    assert!(driver.take_acceptance(second.id, 2));
}

#[test]
fn requires_http_success() {
    let folder = tempfile::tempdir().unwrap();
    let spec = r#"  entrypoint: request
  templates:
  - name: request
    http:
      url: https://example.invalid
      successCondition: response.statusCode == 200
"#;
    let mut driver = driver(folder.path(), spec);
    let task = driver.start().unwrap().remove(0);
    let paths: Vec<(String, String)> = task
        .outputs
        .iter()
        .map(|path| {
            let text = if path.ends_with("response-status") {
                "500"
            } else {
                ""
            };
            (path.clone(), text.to_owned())
        })
        .collect();
    let borrowed: Vec<(&str, &str)> = paths
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    driver
        .complete(output(folder.path(), &task, 1, &borrowed))
        .unwrap();
    assert!(!driver.take_acceptance(task.id, 1));
}
