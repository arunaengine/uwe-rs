//! Direct Argo artifact subpaths select only children of materialized directories.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use workflow_graph::{Driver, Launch, Outcome, OutputFile, Task};

fn driver(folder: &Path, workflow: &str) -> Box<dyn Driver> {
    let target = folder.join("workflow.yaml");
    std::fs::write(&target, workflow).unwrap();
    argo_runtime::driver(&Launch {
        target,
        ..Launch::default()
    })
    .unwrap()
}

fn output(task: &Task, location: &Path) -> Outcome {
    Outcome {
        id: task.id,
        files: vec![OutputFile {
            path: task.outputs[0].clone(),
            location: location.to_string_lossy().into_owned(),
            directory: true,
            ..OutputFile::default()
        }],
        ..Outcome::default()
    }
}

fn directory_workflow(selected: &str) -> String {
    format!(
        r#"apiVersion: argoproj.io/v1alpha1
kind: Workflow
metadata: {{name: subpath}}
spec:
  entrypoint: main
  arguments:
    parameters: [{{name: selected, value: {selected}}}]
  templates:
  - name: main
    steps:
    - - {{name: make, template: make}}
    - - name: use
        template: use
        arguments:
          artifacts:
          - {{name: tree, from: "{{{{steps.make.outputs.artifacts.tree}}}}"}}
  - name: make
    container: {{image: busybox}}
    outputs:
      artifacts:
      - name: tree
        path: /tree
  - name: use
    inputs:
      artifacts:
      - name: tree
        path: /input
        subPath: "{{{{workflow.parameters.selected}}}}"
    container: {{image: busybox}}
"#
    )
}

#[test]
fn selects_directory_subpath() {
    let folder = tempfile::tempdir().unwrap();
    let tree = folder.path().join("tree");
    std::fs::create_dir_all(tree.join("chosen")).unwrap();
    std::fs::write(tree.join("chosen/input.txt"), "selected").unwrap();
    let mut driver = driver(folder.path(), &directory_workflow("chosen/input.txt"));
    let first = driver.start().unwrap();
    let next = driver.complete(output(&first[0], &tree)).unwrap();
    let input = next[0]
        .inputs
        .iter()
        .find(|input| input.path == "/input")
        .unwrap();
    assert_eq!(
        input.source,
        tree.join("chosen/input.txt").to_string_lossy()
    );
    assert!(!input.directory);
}

#[test]
fn rejects_subpath_escapes() {
    for (selected, detail) in [
        ("../outside", "invalid subPath"),
        ("/outside", "invalid subPath"),
        ("escape", "escapes"),
    ] {
        let folder = tempfile::tempdir().unwrap();
        let tree = folder.path().join("tree");
        let outside = folder.path().join("outside");
        std::fs::create_dir(&tree).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, tree.join("escape")).unwrap();
        let mut driver = driver(folder.path(), &directory_workflow(selected));
        let first = driver.start().unwrap();
        let next = driver.complete(output(&first[0], &tree)).unwrap();
        assert!(next.is_empty());
        let message = driver.finish().unwrap_err().message;
        assert!(message.contains(detail), "{message}");
    }
}

#[test]
fn rejects_file_subpaths() {
    for source in [
        "raw: {data: text}",
        "http: {url: 'https://example.org/input?token=x'}",
        "s3: {bucket: data, key: input.txt}",
        "gcs: {bucket: data, key: input.txt}",
    ] {
        let folder = tempfile::tempdir().unwrap();
        let workflow = format!(
            r#"apiVersion: argoproj.io/v1alpha1
kind: Workflow
metadata: {{name: direct}}
spec:
  entrypoint: main
  templates:
  - name: main
    inputs:
      artifacts:
      - name: input
        subPath: child
        {source}
    container: {{image: busybox}}
"#
        );
        let mut driver = driver(folder.path(), &workflow);
        let message = match driver.start() {
            Err(error) => error.message,
            Ok(tasks) => {
                assert!(tasks.is_empty());
                driver.finish().unwrap_err().message
            }
        };
        assert!(message.contains("cannot select subPath child"), "{message}");
    }
}
