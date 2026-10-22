//! Native acceptance gates actual runner cache writes and observed-hit invalidation.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::time::Duration;

use content_store::{Holder, Store, layout};
use executor::manifest::Collected;
use executor::{Backend, ExecutorError, Finished, Job};
use task_model::Digest;
use task_runner::{CacheEntry, Runner, RunnerExecutor, Settings};
use workflow_graph::{Attempt, CancellationToken, ExecuteError, Executor, Network, Task, TaskId};

struct Fake {
    scratch: PathBuf,
    calls: AtomicUsize,
    exit_code: AtomicI32,
}

impl Backend for Fake {
    async fn pin(&self, image: &str) -> Result<Option<String>, ExecutorError> {
        Ok(Some(image.to_owned()))
    }

    async fn run(&self, job: Job<'_>, _: &CancellationToken) -> Result<Finished, ExecutorError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let root = self.scratch.join(job.name);
        std::fs::create_dir_all(root.join("work"))?;
        std::fs::create_dir_all(root.join("control"))?;
        std::fs::write(root.join("work/out"), job.name)?;
        Ok(Finished {
            address: None,
            exit_code: Some(self.exit_code.load(Ordering::SeqCst)),
            files: vec![Collected {
                path: "out".into(),
                directory: false,
            }],
            root,
            usage: None,
        })
    }
}

fn executor(root: &Path, store: Store, run: &str) -> RunnerExecutor<Fake> {
    RunnerExecutor::new(
        Runner::new(
            store,
            Fake {
                scratch: root.join("scratch"),
                calls: AtomicUsize::new(0),
                exit_code: AtomicI32::new(0),
            },
            run,
            Settings {
                downloads: root.join("downloads"),
                import_inputs: false,
                storage: storage::Storage::default(),
                digests: None,
            },
        ),
        None,
    )
}

fn task(id: u64) -> Task {
    Task {
        id: TaskId(id),
        name: "test".into(),
        image: Some(format!("image@sha256:{}", "a".repeat(64))),
        command: vec!["true".into()],
        outputs: vec!["out".into()],
        network: Network::Denied,
        ..Task::default()
    }
}

fn attempt(id: u64, number: u32, cancel: &CancellationToken) -> Attempt {
    Attempt {
        task: Arc::new(task(id)),
        number,
        cancel: cancel.clone(),
    }
}

fn key() -> Digest {
    let task = task(1);
    task_runner::request(&task, task.image.clone(), Vec::new(), None)
        .spec
        .cache_key()
        .unwrap()
}

async fn cached(store: &Store) -> Option<CacheEntry> {
    store
        .get_entry("network-denied", &key())
        .await
        .unwrap()
        .map(|bytes| CacheEntry::parse(&key(), &bytes).unwrap())
}

async fn seed(root: &Path, store: &Store) -> CacheEntry {
    let executor = executor(root, store.clone(), "seed");
    let task = task(1);
    let prepared = executor
        .runner()
        .request(&task, task.image.clone())
        .await
        .unwrap();
    let result = executor
        .runner()
        .execute(&prepared, &CancellationToken::new())
        .await
        .unwrap();
    assert!(!result.reused);
    cached(store)
        .await
        .expect("bare Runner::execute still publishes")
}

#[tokio::test]
async fn acceptance_gates_publication() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::local(&root.path().join("store")).unwrap();
    let mut executor = executor(root.path(), store.clone(), "run");
    let cancel = CancellationToken::new();
    let first = executor.run(attempt(1, 1, &cancel)).await.unwrap();
    assert!(!first.cached);
    assert!(cached(&store).await.is_none());
    executor.settle(TaskId(2), 1, true).await.unwrap();
    drop(executor.settle(TaskId(1), 1, true));
    assert!(cached(&store).await.is_none());
    executor.settle(TaskId(1), 1, false).await.unwrap();
    executor.settle(TaskId(1), 1, true).await.unwrap();
    assert!(cached(&store).await.is_none());
    let second = executor.run(attempt(1, 2, &cancel)).await.unwrap();
    executor.settle(TaskId(1), 1, true).await.unwrap();
    assert!(cached(&store).await.is_none());
    executor.settle(TaskId(1), 2, true).await.unwrap();
    assert_eq!(
        cached(&store).await.unwrap().files[0].digest.to_string(),
        second.files[0].digest.clone().unwrap()
    );
    let reused = executor.run(attempt(2, 1, &cancel)).await.unwrap();
    assert!(reused.cached);
    assert_eq!(executor.runner().backend().calls.load(Ordering::SeqCst), 2);
    executor.settle(TaskId(2), 1, true).await.unwrap();
}

#[tokio::test]
async fn preserves_accepted_code() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::local(&root.path().join("store")).unwrap();
    let mut executor = executor(root.path(), store.clone(), "run");
    executor
        .runner()
        .backend()
        .exit_code
        .store(7, Ordering::SeqCst);
    let cancel = CancellationToken::new();
    let task = task(1);
    let prepared = executor
        .runner()
        .request(&task, task.image.clone())
        .await
        .unwrap();
    let direct = executor.runner().execute(&prepared, &cancel).await.unwrap();
    assert_eq!(direct.exit_code, Some(7));
    assert!(cached(&store).await.is_none());
    let first = executor.run(attempt(1, 1, &cancel)).await.unwrap();
    assert_eq!(first.exit_code, 7);
    executor.settle(TaskId(1), 1, false).await.unwrap();
    assert!(cached(&store).await.is_none());
    executor.run(attempt(1, 2, &cancel)).await.unwrap();
    executor.settle(TaskId(1), 2, true).await.unwrap();
    assert_eq!(cached(&store).await.unwrap().exit_code, 7);
    let reused = executor.run(attempt(1, 3, &cancel)).await.unwrap();
    assert!(reused.cached);
    assert_eq!(reused.exit_code, 7);
    executor.settle(TaskId(1), 3, true).await.unwrap();
    executor.check_reproducibility(1.0);
    let checked = executor.run(attempt(1, 4, &cancel)).await.unwrap();
    assert_eq!(checked.exit_code, 7);
    assert_eq!(
        checked.reproducibility.unwrap().verdict,
        workflow_graph::Verdict::Differs {
            paths: vec!["out".into()]
        }
    );
    executor.settle(TaskId(1), 4, true).await.unwrap();
}

#[test]
fn restores_legacy_code() {
    let bytes = br#"{"run":"old","task":"1","files":[],"stdout":null,"stderr":null}"#;
    let entry = CacheEntry::parse(&key(), bytes).unwrap();
    assert_eq!(entry.result().exit_code, Some(0));
}

#[tokio::test]
async fn removes_observed_version() {
    for layered in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let local = Store::local(&root.path().join("store")).unwrap();
        let store = if layered {
            Store::layered(local, Store::memory().unwrap()).unwrap()
        } else {
            local
        };
        let original = seed(root.path(), &store).await;
        let mut executor = executor(root.path(), store.clone(), "consumer");
        let cancel = CancellationToken::new();
        assert!(executor.run(attempt(1, 1, &cancel)).await.unwrap().cached);
        let observed = store.observe_entry("", &key()).await.unwrap().unwrap();
        assert!(store.invalidate_entry("", &key(), &observed).await.unwrap());
        let replacement = CacheEntry {
            run: "replacement".into(),
            ..original
        };
        store
            .put_entry(
                "network-denied",
                &key(),
                replacement.to_bytes(&key()).unwrap().into(),
            )
            .await
            .unwrap();
        executor.settle(TaskId(1), 1, false).await.unwrap();
        assert_eq!(cached(&store).await.unwrap(), replacement);
        if let Some(shared) = store.shared() {
            assert_eq!(cached(shared).await.unwrap(), replacement);
        }
        assert!(executor.run(attempt(2, 1, &cancel)).await.unwrap().cached);
        executor.settle(TaskId(2), 1, false).await.unwrap();
        assert!(cached(&store).await.is_none());
        if let Some(shared) = store.shared() {
            assert!(cached(shared).await.is_none());
        }
    }
}

#[tokio::test]
async fn rejects_earlier_result() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::local(&root.path().join("store")).unwrap();
    let earlier = seed(root.path(), &store).await;
    let mut executor = executor(root.path(), store.clone(), "comparison");
    executor.check_reproducibility(1.0);
    let found = executor
        .run(attempt(1, 1, &CancellationToken::new()))
        .await
        .unwrap();
    assert!(found.cached && found.reproducibility.is_some());
    assert_eq!(
        found.files[0].digest,
        Some(earlier.files[0].digest.to_string())
    );
    assert_eq!(executor.runner().backend().calls.load(Ordering::SeqCst), 1);
    executor.settle(TaskId(1), 1, false).await.unwrap();
    assert!(cached(&store).await.is_none());
}

#[tokio::test]
async fn releases_build_leases() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::local(&root.path().join("store")).unwrap();
    let mut executor = executor(root.path(), store.clone(), "ordered");
    let cancel = CancellationToken::new();
    // The later ordered outcome is buffered while the earlier task still needs this key.
    let later = executor.run(attempt(2, 1, &cancel)).await.unwrap();
    let name = format!(
        "tasks/{}/{}",
        layout::namespace_name("network-denied"),
        key().hex()
    );
    let free = store
        .try_lease(&name, &Holder::new("probe", "released"))
        .await
        .unwrap();
    assert!(free.is_some());
    drop(free);
    let earlier =
        tokio::time::timeout(Duration::from_secs(2), executor.run(attempt(1, 1, &cancel)))
            .await
            .unwrap()
            .unwrap();
    assert!(!earlier.cached && !later.cached);
    assert!(cached(&store).await.is_none());
    executor.settle(TaskId(1), 1, true).await.unwrap();
    executor.settle(TaskId(2), 1, true).await.unwrap();
    assert_eq!(cached(&store).await.unwrap().task, "1");
}

#[tokio::test]
async fn skips_contended_settlement() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::local(&root.path().join("store")).unwrap();
    let mut executor = executor(root.path(), store.clone(), "contended");
    executor
        .run(attempt(1, 1, &CancellationToken::new()))
        .await
        .unwrap();
    let name = format!(
        "tasks/{}/{}",
        layout::namespace_name("network-denied"),
        key().hex()
    );
    let _owner = store
        .try_lease(&name, &Holder::new("other", "build"))
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), executor.settle(TaskId(1), 1, true))
        .await
        .unwrap()
        .unwrap();
    assert!(cached(&store).await.is_none());
}

#[tokio::test(start_paused = true)]
async fn waits_out_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::local(&root.path().join("store")).unwrap();
    let mut executor = executor(root.path(), store.clone(), "run");
    executor
        .run(attempt(1, 1, &CancellationToken::new()))
        .await
        .unwrap();
    let guard = task_runner::clean::maintenance(&store).await.unwrap();
    let mut settling = tokio::spawn(executor.settle(TaskId(1), 1, true));
    // Clean-up holds the gate for longer than settlement may take once it has the gate.
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert!(!settling.is_finished());
    drop(guard);
    (&mut settling).await.unwrap().unwrap();
    assert_eq!(cached(&store).await.unwrap().task, "1");
}

#[tokio::test]
async fn sibling_shares_gate() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::local(&root.path().join("store")).unwrap();
    let mut executor = executor(root.path(), store.clone(), "run");
    let cancel = CancellationToken::new();
    executor.run(attempt(1, 1, &cancel)).await.unwrap();
    // Another task holds the gate while it stages a large input.
    let staging = store.try_shared_lease("maintenance").await.unwrap();
    assert!(staging.is_some());
    let mut other = task(2);
    other.command = vec!["other".into()];
    let other = Attempt {
        task: Arc::new(other),
        number: 1,
        cancel: cancel.clone(),
    };
    let ran = tokio::time::timeout(Duration::from_secs(5), executor.run(other)).await;
    assert!(!ran.unwrap().unwrap().cached);
    let settled = tokio::time::timeout(Duration::from_secs(5), executor.settle(TaskId(1), 1, true));
    settled.await.unwrap().unwrap();
    assert_eq!(cached(&store).await.unwrap().task, "1");
    assert!(task_runner::clean::maintenance(&store).await.is_err());
    drop(staging);
}

#[tokio::test]
async fn skips_failed_write() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::local(&root.path().join("store")).unwrap();
    let mut executor = executor(root.path(), store.clone(), "run");
    executor
        .run(attempt(1, 1, &CancellationToken::new()))
        .await
        .unwrap();
    // A file where the leases of tasks go makes the write fail.
    let leases = root.path().join("store/locks/tasks");
    std::fs::remove_dir_all(&leases).unwrap();
    std::fs::write(&leases, "blocked").unwrap();
    executor.settle(TaskId(1), 1, true).await.unwrap();
    std::fs::remove_file(&leases).unwrap();
    assert!(cached(&store).await.is_none());
}

#[tokio::test]
async fn rejects_superseded_publication() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::local(&root.path().join("store")).unwrap();
    let mut cancelled = executor(root.path(), store.clone(), "cancelled");
    let cancel = CancellationToken::new();
    cancelled.run(attempt(1, 1, &cancel)).await.unwrap();
    cancel.cancel();
    assert!(matches!(
        cancelled.settle(TaskId(1), 1, true).await,
        Err(ExecuteError::Cancelled)
    ));
    assert!(cached(&store).await.is_none());

    let mut dropped = executor(root.path(), store.clone(), "dropped");
    dropped
        .run(attempt(1, 1, &CancellationToken::new()))
        .await
        .unwrap();
    let approval = dropped.settle(TaskId(1), 1, true);
    drop(dropped);
    assert!(matches!(approval.await, Err(ExecuteError::Cancelled)));
    assert!(cached(&store).await.is_none());

    let mut superseded = executor(root.path(), store.clone(), "superseded");
    let cancel = CancellationToken::new();
    superseded.run(attempt(1, 1, &cancel)).await.unwrap();
    let old = superseded.settle(TaskId(1), 1, true);
    let latest = superseded.run(attempt(1, 2, &cancel)).await.unwrap();
    assert!(matches!(old.await, Err(ExecuteError::Cancelled)));
    superseded.settle(TaskId(1), 1, true).await.unwrap();
    assert!(cached(&store).await.is_none());
    superseded.settle(TaskId(1), 2, true).await.unwrap();
    assert_eq!(
        cached(&store).await.unwrap().files[0].digest.to_string(),
        latest.files[0].digest.clone().unwrap()
    );
}

#[tokio::test]
async fn releases_terminal_payloads() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::local(&root.path().join("store")).unwrap();
    let mut executor = executor(root.path(), store, "retention");
    let mut live = task(100);
    live.command = vec!["live".into()];
    let live = Arc::new(live);
    let live_probe = Arc::downgrade(&live);
    executor
        .run(Attempt {
            task: live,
            number: 1,
            cancel: CancellationToken::new(),
        })
        .await
        .unwrap();
    let held = executor.settle(TaskId(100), 1, true);

    for id in 1..=18 {
        let mode = id % 6;
        let mut task = task(id);
        let cancel = CancellationToken::new();
        if mode == 2 {
            task.reuse = workflow_graph::Reuse::Disabled;
        } else if mode == 3 {
            task.inputs.push(workflow_graph::Input {
                path: "missing".into(),
                source: root.path().join("missing").to_string_lossy().into_owned(),
                ..Default::default()
            });
        } else if mode == 4 {
            cancel.cancel();
        }
        let task = Arc::new(task);
        let probe = Arc::downgrade(&task);
        let running = executor.run(Attempt {
            task,
            number: 1,
            cancel,
        });
        if mode == 5 {
            drop(running);
        } else {
            let result = running.await;
            if matches!(mode, 3 | 4) {
                assert!(result.is_err());
            } else {
                result.unwrap();
                if mode != 2 {
                    executor.settle(TaskId(id), 1, mode == 0).await.unwrap();
                }
            }
        }
        executor.settle(TaskId(999), 1, false).await.unwrap();
        assert!(probe.upgrade().is_none(), "retained terminal attempt {id}");
    }
    // Its candidate was taken by an unpolled settlement, not completed or abandoned.
    drop(held);
    assert!(live_probe.upgrade().is_some());
    executor.settle(TaskId(100), 1, true).await.unwrap();
    executor.settle(TaskId(999), 1, false).await.unwrap();
    assert!(live_probe.upgrade().is_none());
}
