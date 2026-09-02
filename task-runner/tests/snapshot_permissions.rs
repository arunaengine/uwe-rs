//! Snapshot mounts expose readable private copies while their host outer folder stays private.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use content_store::Store;
use executor::{Backend, ExecutorError, Finished, Job, Staging};
use task_model::{Digest, Tree, TreeEntry};
use task_runner::{Runner, Settings};
use tokio_util::sync::CancellationToken;
use workflow_graph::{Input, Reuse, Task, Writable};

struct Inspect {
    root: PathBuf,
    staged: Mutex<Vec<Staging>>,
}

impl Backend for Inspect {
    async fn pin(&self, _: &str) -> Result<Option<String>, ExecutorError> {
        Ok(None)
    }

    async fn run(&self, job: Job<'_>, _: &CancellationToken) -> Result<Finished, ExecutorError> {
        self.staged.lock().unwrap().push(job.staging.clone());
        let root = self.root.join(job.name);
        std::fs::create_dir_all(root.join("control"))?;
        Ok(Finished {
            root,
            exit_code: Some(0),
            files: Vec::new(),
            usage: None,
        })
    }
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn set_mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn snapshot_permissions_under_restrictive_umask() {
    if std::env::var_os("IUNCTUS_SNAPSHOT_UMASK_TEST").is_none() {
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg("umask 077; exec \"$1\" --exact snapshot_permissions_under_restrictive_umask")
            .arg("snapshot-permissions")
            .arg(std::env::current_exe().unwrap())
            .env("IUNCTUS_SNAPSHOT_UMASK_TEST", "1")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let root = tempfile::tempdir().unwrap();
            let downloads = root.path().join("downloads");
            std::fs::create_dir(&downloads).unwrap();
            set_mode(&downloads, 0o750);
            let outer = downloads.join("input-snapshots");
            std::fs::create_dir(&outer).unwrap();
            set_mode(&outer, 0o755);
            assert_eq!(
                std::fs::metadata(&outer).unwrap().uid(),
                std::fs::metadata(root.path()).unwrap().uid()
            );
            let source = root.path().join("source");
            std::fs::create_dir_all(source.join("nested/empty")).unwrap();
            std::fs::write(source.join("plain"), b"private").unwrap();
            std::fs::write(source.join("nested/program"), b"#!/bin/sh\n").unwrap();
            set_mode(&source, 0o700);
            set_mode(&source.join("nested"), 0o700);
            set_mode(&source.join("plain"), 0o600);
            set_mode(&source.join("nested/program"), 0o710);
            std::os::unix::fs::symlink("nested", source.join("selected")).unwrap();
            std::os::unix::fs::symlink("..", source.join("nested/back")).unwrap();
            let backend = Inspect {
                root: root.path().join("scratch"),
                staged: Mutex::new(Vec::new()),
            };
            let runner = Runner::new(
                Store::local(&root.path().join("store")).unwrap(),
                backend,
                "permissions",
                Settings {
                    downloads,
                    import_inputs: false,
                    storage: storage::Storage::default(),
                    digests: None,
                },
            );
            let task = Task {
                command: vec!["true".into()],
                reuse: Reuse::Disabled,
                inputs: vec![
                    Input {
                        path: "plain".into(),
                        source: source.join("plain").to_string_lossy().into_owned(),
                        ..Input::default()
                    },
                    Input {
                        path: "tree".into(),
                        source: source.to_string_lossy().into_owned(),
                        directory: true,
                        ..Input::default()
                    },
                    Input {
                        path: "program".into(),
                        source: source.join("nested/program").to_string_lossy().into_owned(),
                        ..Input::default()
                    },
                    Input {
                        path: "mutable".into(),
                        source: source.join("plain").to_string_lossy().into_owned(),
                        writable: Writable::InPlace,
                        ..Input::default()
                    },
                ],
                ..Task::default()
            };
            let prepared = runner.request(&task, Some("debian".into())).await.unwrap();
            assert_eq!(
                prepared.request.spec.inputs[0].digest,
                Digest::sha256(b"private")
            );
            assert!(prepared.request.spec.inputs[2].executable);
            runner
                .execute(&prepared, &CancellationToken::new())
                .await
                .unwrap();
            {
                let staged = runner.backend().staged.lock().unwrap();
                let staged = &staged[0].inputs;
                assert_eq!(mode(&staged[0].source), 0o644);
                assert_eq!(mode(staged[0].source.parent().unwrap()), 0o755);
                assert!(!staged[0].copy);
                let tree = &staged[1].source;
                for path in ["", "nested", "nested/empty"] {
                    assert_eq!(mode(&tree.join(path)), 0o755);
                }
                assert_eq!(mode(&tree.join("plain")), 0o644);
                assert_eq!(mode(&tree.join("nested/program")), 0o755);
                assert_eq!(
                    std::fs::read_link(tree.join("selected")).unwrap(),
                    Path::new("nested")
                );
                assert_eq!(
                    std::fs::read_link(tree.join("nested/back")).unwrap(),
                    Path::new("..")
                );
                assert_eq!(std::fs::read(tree.join("plain")).unwrap(), b"private");
                assert_eq!(mode(&staged[2].source), 0o755);
                assert!(staged[3].in_place);
                assert_eq!(staged[3].source, source.join("plain"));
                assert_eq!(mode(&staged[3].source), 0o600);
            }
            assert_eq!(mode(&outer), 0o700);
            assert_eq!(mode(outer.parent().unwrap()), 0o750);
            assert_eq!(mode(&source), 0o700);
            assert_eq!(mode(&source.join("nested")), 0o700);
            assert_eq!(mode(&source.join("plain")), 0o600);
            assert_eq!(mode(&source.join("nested/program")), 0o710);

            let content = runner.import_bytes(b"stored").await.unwrap();
            let tree = Tree {
                entries: vec![
                    TreeEntry::File {
                        path: "nested/data".into(),
                        digest: content,
                        size: 6,
                        executable: false,
                    },
                    TreeEntry::EmptyDirectory {
                        path: "deep/empty".into(),
                    },
                    TreeEntry::Symlink {
                        path: "selected".into(),
                        target: "nested".into(),
                    },
                ],
            };
            let digest = runner.import_tree(&tree).await.unwrap();
            let task = Task {
                command: vec!["true".into()],
                reuse: Reuse::Disabled,
                inputs: vec![Input {
                    path: "stored".into(),
                    directory: true,
                    ..Input::default()
                }],
                ..Task::default()
            };
            let mut prepared = runner.request(&task, Some("debian".into())).await.unwrap();
            prepared.request.spec.inputs[0].digest = digest;
            runner
                .execute(&prepared, &CancellationToken::new())
                .await
                .unwrap();
            let staged = runner.backend().staged.lock().unwrap();
            let tree = &staged[1].inputs[0].source;
            for path in ["", "nested", "deep", "deep/empty"] {
                assert_eq!(mode(&tree.join(path)), 0o755);
            }
            assert_eq!(mode(&tree.join("nested/data")), 0o644);
            assert_eq!(
                std::fs::read_link(tree.join("selected")).unwrap(),
                Path::new("nested")
            );
            assert_eq!(
                std::fs::read(tree.join("selected/data")).unwrap(),
                b"stored"
            );
            assert_eq!(mode(&outer), 0o700);
        });
}
