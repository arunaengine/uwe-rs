//! Caller lease loss cancels work without publishing late results or removing journals.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

// The futures of attempts nest deeply, deeper than the default limit lets the compiler check.
#![recursion_limit = "256"]

use std::path::{Path, PathBuf};
use std::time::Duration;

use content_store::{Holder, Store, layout};
use executor::{Backend, ExecutorError, Finished, Job};
use task_runner::{CacheEntry, Runner, Settings};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use workflow_graph::{Task, TaskId};

struct Waiting {
    root: PathBuf,
    started: Notify,
}

impl Backend for Waiting {
    async fn pin(&self, image: &str) -> Result<Option<String>, ExecutorError> {
        Ok(Some(image.into()))
    }
    async fn run(
        &self,
        job: Job<'_>,
        cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        let root = self.root.join(job.name);
        std::fs::create_dir_all(&root)?;
        std::fs::write(root.join("remote.json"), "unresolved")?;
        self.started.notify_one();
        cancel.cancelled().await;
        std::fs::write(root.join("cleanup-observed"), "cancelled")?;
        Ok(Finished {
            exit_code: Some(0),
            files: Vec::new(),
            root,
            usage: None,
        })
    }
}

fn runner(root: &Path, store: Store) -> Runner<Waiting> {
    Runner::new(
        store,
        Waiting {
            root: root.join("backend"),
            started: Notify::new(),
        },
        "run",
        Settings {
            downloads: root.join("downloads"),
            import_inputs: false,
            storage: storage::Storage::default(),
            digests: None,
        },
    )
}

fn task() -> Task {
    Task {
        id: TaskId(1),
        command: vec!["true".into()],
        ..Task::default()
    }
}

#[tokio::test]
async fn detachment_blocks_admission() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::local(&root.path().join("store")).unwrap();
    let runner = runner(root.path(), store.clone());
    let prepared = runner
        .request(&task(), Some("image@sha256:00".into()))
        .await
        .unwrap();
    let key = prepared.request.spec.cache_key().unwrap();
    let entry = CacheEntry::parse(
        &key,
        br#"{"run":"earlier","task":"1","files":[],"stdout":null,"stderr":null}"#,
    )
    .unwrap();
    store
        .put_entry("", &key, entry.to_bytes(&key).unwrap().into())
        .await
        .unwrap();
    let reused = runner
        .execute(&prepared, &CancellationToken::new())
        .await
        .unwrap();
    assert!(reused.reused);
    let location = layout::lease("runs/run").unwrap();
    std::fs::remove_file(store.local_path(&location).unwrap()).unwrap();
    let cloned = runner.clone();
    let error = cloned
        .execute(&prepared, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("lease"));
    assert!(
        cloned
            .request(&task(), Some("image@sha256:00".into()))
            .await
            .is_err()
    );
    assert!(cloned.import_bytes(b"later input").await.is_err());
    assert!(!root.path().join("backend").exists());
}

#[tokio::test]
async fn rejects_late_success() {
    for case in [
        "local-task",
        "memory-expiry",
        "memory-takeover",
        "user-cancel",
    ] {
        let root = tempfile::tempdir().unwrap();
        let operator = opendal::Operator::new(opendal::services::Memory::default()).unwrap();
        let store = if case.starts_with("memory") {
            Store::remote(operator.clone(), "memory://lease-loss").unwrap()
        } else {
            Store::local(&root.path().join("store")).unwrap()
        };
        let runner = std::sync::Arc::new(runner(root.path(), store.clone()));
        let prepared = runner
            .request(&task(), Some("image@sha256:00".into()))
            .await
            .unwrap();
        let key = prepared.request.spec.cache_key().unwrap();
        let cancel = CancellationToken::new();
        let (owned, prepared, stop) = (runner.clone(), prepared.clone(), cancel.clone());
        let execution = tokio::spawn(async move { owned.execute(&prepared, &stop).await });
        tokio::time::timeout(Duration::from_secs(2), runner.backend().started.notified())
            .await
            .unwrap();
        let name = if case == "memory-takeover" {
            "runs/run".to_owned()
        } else {
            format!("tasks/default/{}", key.hex())
        };
        let location = layout::lease(&name).unwrap();
        if case == "user-cancel" {
            cancel.cancel();
        } else if case.starts_with("memory") {
            let mut record: serde_json::Value =
                serde_json::from_slice(&operator.read(&location).await.unwrap().to_bytes())
                    .unwrap();
            record["expires"] = serde_json::json!(0);
            operator
                .write(&location, serde_json::to_vec(&record).unwrap())
                .await
                .unwrap();
        } else {
            std::fs::remove_file(store.local_path(&location).unwrap()).unwrap();
        }
        let successor = if case == "local-task" || case == "memory-takeover" {
            Some(
                store
                    .try_lease(&name, &Holder::new("successor", "task"))
                    .await
                    .unwrap()
                    .unwrap(),
            )
        } else {
            None
        };
        let error = tokio::time::timeout(Duration::from_secs(3), execution)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(
            error.to_string().contains(if case == "user-cancel" {
                "cancelled"
            } else {
                "lease"
            }),
            "{case}: {error}"
        );
        assert!(store.get_entry("", &key).await.unwrap().is_none(), "{case}");
        let scratch = root.path().join("backend/run-1-1");
        assert!(scratch.join("remote.json").is_file(), "{case}");
        assert!(scratch.join("cleanup-observed").is_file(), "{case}");
        if let Some(successor) = successor {
            tokio::time::sleep(Duration::from_millis(50)).await;
            successor.check().await.unwrap();
        }
        if case != "user-cancel" {
            assert!(runner.import_bytes(b"after loss").await.is_err(), "{case}");
        }
    }
}
