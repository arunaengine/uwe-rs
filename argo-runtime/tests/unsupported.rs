//! Unsupported execution controls must fail before any task is published.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{DriverError, Launch};

fn start_error(spec: &str) -> DriverError {
    let folder = tempfile::tempdir().expect("temporary folder");
    let target = folder.path().join("workflow.yaml");
    std::fs::write(
        &target,
        format!("apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {{name: controls}}\nspec:\n  entrypoint: main\n{spec}"),
    ).expect("workflow written");
    let mut driver = argo_runtime::driver(&Launch {
        target,
        ..Launch::default()
    })
    .expect("driver");
    driver.start().expect_err("unsupported control must fail")
}

#[test]
fn rejects_unsupported_workflow_controls() {
    for field in [
        "synchronization: {mutexes: [{name: lock}]}",
        "activeDeadlineSeconds: 5",
        "artifactRepositoryRef: {configMap: artifacts}",
        "podSpecPatch: '{}'",
        "podResources: {}",
        "shutdown: Stop",
        "suspend: true",
    ] {
        let error = start_error(&format!(
            "  {field}\n  templates:\n  - name: main\n    container: {{image: alpine}}\n"
        ));
        assert!(error.unsupported, "{field}: {error}");
        assert!(
            error.message.contains(field.split(':').next().unwrap()),
            "{error}"
        );
    }
}

#[test]
fn rejects_unsupported_template_controls_and_defaults() {
    for field in [
        "synchronization: {mutexes: [{name: lock}]}",
        "memoize: {key: cache, cache: {configMap: {name: cache}}}",
        "podSpecPatch: '{}'",
        "podResources: {}",
        "resourceClaims: []",
        "pendingTimeout: 5s",
        "archiveLocation: {s3: {bucket: outputs}}",
    ] {
        for spec in [
            format!(
                "  templates:\n  - name: main\n    {field}\n    container: {{image: alpine}}\n"
            ),
            format!(
                "  templateDefaults:\n    {field}\n  templates:\n  - name: main\n    container: {{image: alpine}}\n"
            ),
        ] {
            let error = start_error(&spec);
            assert!(error.unsupported, "{field}: {error}");
            assert!(
                error.message.contains(field.split(':').next().unwrap()),
                "{error}"
            );
        }
    }
    assert!(start_error("  templates:\n  - name: main\n    suspend: {duration: 5s}\n").unsupported);
}

#[test]
fn rejects_unsupported_container_lifecycle_controls() {
    for field in [
        "lifecycle",
        "livenessProbe",
        "readinessProbe",
        "startupProbe",
        "restartPolicy",
        "resizePolicy",
    ] {
        let error = start_error(&format!(
            "  templates:\n  - name: main\n    container: {{image: alpine, {field}: {{}}}}\n"
        ));
        assert!(error.unsupported, "{field}: {error}");
        assert!(error.message.contains(field), "{error}");
    }
}
