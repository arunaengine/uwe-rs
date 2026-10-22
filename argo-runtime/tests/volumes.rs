//! Checks authorized host paths, explicit backing volumes and container mount conflicts.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use argo_runtime::{Policy, Services};
use workflow_graph::{DriverError, Launch, Task};

fn start(folder: &Path, source: &str, policy: Policy) -> Result<Vec<Task>, DriverError> {
    let target = folder.join("workflow.yaml");
    std::fs::write(&target, format!("apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {{name: volumes}}\nspec:\n  entrypoint: main\n  volumes:\n  - name: data\n    {source}\n  templates:\n  - name: main\n    container:\n      image: alpine\n      volumeMounts: [{{name: data, mountPath: /data, readOnly: true}}]\n")).unwrap();
    argo_runtime::driver_with(
        &Launch {
            target,
            ..Launch::default()
        },
        Services {
            policy,
            ..Services::default()
        },
    )?
    .start()
}

#[test]
fn confines_host_paths() {
    let folder = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let policy = Policy {
        host_roots: Some(vec![folder.path().to_owned()]),
        ..Policy::default()
    };
    let source = |path: &Path| format!("hostPath: {{path: '{}', type: Directory}}", path.display());
    assert!(start(folder.path(), &source(folder.path()), policy.clone()).is_ok());
    let failure = start(folder.path(), &source(outside.path()), policy.clone()).unwrap_err();
    assert!(failure.unsupported && failure.message.contains("authorized roots"));
    let link = folder.path().join("escape");
    std::os::unix::fs::symlink(outside.path(), &link).unwrap();
    assert!(start(folder.path(), &source(&link), policy).is_err());
    assert!(start(folder.path(), &source(outside.path()), Policy::default()).is_ok());
    assert!(
        start(
            folder.path(),
            &source(folder.path()),
            Policy {
                host_roots: Some(Vec::new()),
                ..Policy::default()
            }
        )
        .is_err()
    );
}

#[test]
fn requires_volume_mappings() {
    let folder = tempfile::tempdir().unwrap();
    let backing = tempfile::tempdir().unwrap();
    std::fs::write(backing.path().join("reference"), "data").unwrap();
    for source in [
        "persistentVolumeClaim: {claimName: reference}",
        "nfs: {server: example, path: /reference}",
    ] {
        let failure = start(folder.path(), source, Policy::default()).unwrap_err();
        assert!(failure.unsupported && failure.message.contains("explicit host volume mapping"));
        let policy = Policy {
            host_roots: Some(Vec::new()),
            volumes: [("data".into(), backing.path().to_owned())].into(),
        };
        let tasks = start(folder.path(), source, policy).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(
            tasks[0].inputs[0].source,
            backing.path().canonicalize().unwrap().to_string_lossy()
        );
    }
}

#[test]
fn checks_mount_views() {
    let folder = tempfile::tempdir().unwrap();
    for (second, conflict) in [
        ("{name: other, mountPath: /data}", true),
        ("{name: data, mountPath: /data, readOnly: true}", true),
        ("{name: data, mountPath: /data, subPath: different}", true),
        ("{name: data, mountPath: /data, readOnly: false}", false),
        ("{name: other, mountPath: /other}", false),
    ] {
        for set in [false, true] {
            let containers = if set {
                format!(
                    "    containerSet:\n      containers:\n      - name: main\n        image: alpine\n        volumeMounts: [{{name: data, mountPath: /data}}]\n      - name: other\n        image: alpine\n        volumeMounts: [{second}]\n"
                )
            } else {
                format!(
                    "    container:\n      image: alpine\n      volumeMounts: [{{name: data, mountPath: /data}}]\n    sidecars:\n    - name: other\n      image: alpine\n      volumeMounts: [{second}]\n"
                )
            };
            let target = folder.path().join("workflow.yaml");
            std::fs::write(&target, format!("apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {{name: mounts}}\nspec:\n  entrypoint: main\n  volumes: [{{name: data, emptyDir: {{}}}}, {{name: other, emptyDir: {{}}}}]\n  templates:\n  - name: main\n{containers}")).unwrap();
            let mut run = argo_runtime::driver(&Launch {
                target,
                ..Launch::default()
            })
            .unwrap();
            let result = run.start();
            if conflict {
                let failure = result.unwrap_err();
                assert!(
                    failure.unsupported && failure.message.contains("per-container mount views")
                );
            } else {
                assert_eq!(result.unwrap().len(), 1);
            }
        }
    }
}

#[test]
fn retains_created_claims() {
    let folder = tempfile::tempdir().unwrap();
    let target = folder.path().join("workflow.yaml");
    std::fs::write(&target, "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {name: claim}\nspec:\n  entrypoint: main\n  volumeClaimTemplates:\n  - metadata: {name: backing}\n    spec: {}\n  volumes: [{name: alias, persistentVolumeClaim: {claimName: backing}}]\n  templates:\n  - name: main\n    container:\n      image: alpine\n      volumeMounts: [{name: alias, mountPath: /data}]\n").unwrap();
    let mut run = argo_runtime::driver(&Launch {
        target,
        ..Launch::default()
    })
    .unwrap();
    assert_eq!(run.start().unwrap().len(), 1);
}
