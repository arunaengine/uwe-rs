//! Loading Argo documents reports typed fields that the runtime does not recognize.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::Launch;

#[test]
fn rejects_unknown_fields() {
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
      outputs: {}
    templateRef: {name: other, template: main}
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
    assert!(
        error
            .message
            .contains("spec.templates[0].container.outputs")
    );
    assert!(error.message.contains("spec.templates[0].templateRef"));
}

#[test]
fn accepts_collection_fields() {
    let directory = tempfile::tempdir().expect("a folder");
    let target = directory.path().join("workflow.yaml");
    std::fs::write(
        &target,
        "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {name: gc}\nspec:\n  entrypoint: main\n  podGC: {strategy: OnPodCompletion}\n  artifactGC: {strategy: OnWorkflowCompletion}\n  volumeClaimGC: {strategy: OnWorkflowSuccess}\n  templates:\n  - name: main\n    container: {image: alpine}\n    outputs:\n      artifacts: [{name: result, path: /out, artifactGC: {strategy: Never}}]\n",
    )
    .expect("writes the workflow");
    let mut driver = argo_runtime::driver(&Launch {
        target,
        ..Launch::default()
    })
    .expect("driver");
    driver.start().expect("garbage collection is a no-op");
}

#[test]
fn allows_extensible_payloads() {
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

#[test]
fn resolves_namespace_objects() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("library.yaml");
    std::fs::write(&path, "apiVersion: v1\nkind: ConfigMap\nmetadata: {name: shared, namespace: a}\ndata: {value: a}\n---\napiVersion: v1\nkind: ConfigMap\nmetadata: {name: shared, namespace: b}\ndata: {value: b}\n---\napiVersion: argoproj.io/v1alpha1\nkind: ClusterWorkflowTemplate\nmetadata: {name: global}\nspec: {}\n").unwrap();
    let library = argo_runtime::Library::new();
    library.add_documents(0, argo_runtime::load_file(&path).unwrap());
    for namespace in ["a", "b"] {
        let found = library
            .find_in(argo_runtime::Kind::ConfigMap, "shared", &path, namespace)
            .unwrap();
        assert_eq!(found.namespace(), namespace);
        assert_eq!(
            library
                .all_in(argo_runtime::Kind::ConfigMap, &path, namespace)
                .len(),
            1
        );
        assert!(
            library
                .find_in(
                    argo_runtime::Kind::ClusterWorkflowTemplate,
                    "global",
                    &path,
                    namespace
                )
                .is_some()
        );
    }
    assert!(
        library
            .find_in(argo_runtime::Kind::ConfigMap, "shared", &path, "other")
            .is_none()
    );
}

#[test]
fn confines_directory_discovery() {
    let folder = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(folder.path().join("local.yaml"), "local").unwrap();
    std::fs::write(outside.path().join("external.yaml"), "external").unwrap();
    std::os::unix::fs::symlink(outside.path(), folder.path().join("escape")).unwrap();
    std::os::unix::fs::symlink(folder.path(), folder.path().join("cycle")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("external.yaml"),
        folder.path().join("link.yaml"),
    )
    .unwrap();
    let mut files = Vec::new();
    argo_runtime::library::collect_files(folder.path(), &mut files);
    assert_eq!(files, vec![folder.path().join("local.yaml")]);
}

#[test]
fn runs_namespaced_references() {
    let folder = tempfile::tempdir().unwrap();
    let config = folder.path().join("library.yaml");
    let mut documents = String::new();
    for namespace in ["a", "b"] {
        documents.push_str(&format!("---\napiVersion: argoproj.io/v1alpha1\nkind: WorkflowTemplate\nmetadata: {{name: shared, namespace: {namespace}}}\nspec:\n  templates:\n  - name: leaf\n    container:\n      image: {namespace}\n      env: [{{name: VALUE, valueFrom: {{configMapKeyRef: {{name: shared, key: value}}}}}}]\n---\napiVersion: v1\nkind: ConfigMap\nmetadata: {{name: shared, namespace: {namespace}}}\ndata: {{value: {namespace}}}\n"));
    }
    std::fs::write(&config, documents).unwrap();
    for namespace in ["a", "b"] {
        let target = folder.path().join("workflow.yaml");
        std::fs::write(&target, format!("apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {{name: run, namespace: {namespace}}}\nspec:\n  entrypoint: main\n  templates:\n  - name: main\n    steps:\n    - - name: call\n        templateRef: {{name: shared, template: leaf}}\n")).unwrap();
        let mut run = argo_runtime::driver(&Launch {
            target,
            configs: vec![config.clone()],
            ..Launch::default()
        })
        .unwrap();
        let tasks = run.start().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].image.as_deref(), Some(namespace));
        assert_eq!(tasks[0].environment["VALUE"], namespace);
    }
}
