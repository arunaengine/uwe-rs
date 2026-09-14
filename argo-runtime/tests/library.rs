//! Loading Argo documents reports typed fields that the runtime does not recognize.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::Launch;

#[test]
fn unknown_typed_fields_fail_driver_admission_with_their_location() {
    let directory = tempfile::tempdir().expect("a folder");
    let file = directory.path().join("workflow.yaml");
    let workflow = r#"apiVersion: argoproj.io/v1alpha1
kind: Workflow
metadata: {name: typo}
spec:
  entrypoint: main
  paralellism: 2
  templates:
  - name: main
    container:
      image: busybox
      imag: typo
"#;
    std::fs::write(&file, workflow).expect("writes the workflow");
    let launch = Launch {
        target: file.clone(),
        ..Launch::default()
    };
    let error = argo_runtime::driver(&launch)
        .err()
        .expect("unknown fields fail admission");
    assert!(
        error.message.contains(&file.display().to_string()),
        "{}",
        error.message
    );
    assert!(error.message.contains(":6:3:"), "{}", error.message);
    assert!(
        error.message.contains("spec.paralellism"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("spec.templates[0].container.imag"),
        "{}",
        error.message
    );
}

#[test]
fn metadata_and_raw_kubernetes_payloads_remain_extensible() {
    let directory = tempfile::tempdir().expect("a folder");
    let file = directory.path().join("workflow.yaml");
    let workflow = r#"apiVersion: argoproj.io/v1alpha1
kind: Workflow
metadata:
  name: extensions
  custom.example.org/value: retained
spec:
  entrypoint: main
  podMetadata: {custom.example.org/value: retained}
  templates:
  - name: main
    metadata: {custom.example.org/value: retained}
    container:
      image: busybox
      resources:
        requests: {custom.example.org/device: 1}
"#;
    std::fs::write(&file, workflow).expect("writes the workflow");
    let documents = argo_runtime::load_file(&file).expect("raw payload fields remain accepted");
    assert_eq!(documents.len(), 1);
}
