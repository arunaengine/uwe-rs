//! Checks the cache, the staging and the stored outputs of the runner with a fake executor.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

// The futures of attempts nest deeply, deeper than the default limit lets the compiler check.
#![recursion_limit = "256"]

use std::path::Path;
use std::sync::Mutex;

use content_store::Store;
use executor::manifest::Collected;
use executor::{Backend, ExecutorError, Finished, Job, Staging};
use task_model::FileKind;
use task_runner::{Prepared, Runner, Settings, TaskResult};
use tokio_util::sync::CancellationToken;
use workflow_graph::{Input, Task, TaskId};

/// Writes each output pattern as a file with the first staged file's content, or for a pattern
/// that ends with `/` as a directory with that file and an empty folder. It prints `said` and
/// remembers each run and what it staged.
struct Fake {
    scratch: std::path::PathBuf,
    runs: Mutex<Vec<String>>,
    staged: Mutex<Vec<Staging>>,
    policies: Mutex<Vec<(bool, Option<u64>)>>,
    exit_code: i32,
    /// How long each task takes.
    delay: std::time::Duration,
    /// Whether each run writes other bytes, like a tool that writes the time.
    varies: bool,
}

impl Backend for Fake {
    async fn pin(&self, image: &str) -> Result<Option<String>, ExecutorError> {
        Ok(Some(format!("{image}@sha256:00")))
    }

    async fn run(
        &self,
        job: Job<'_>,
        _cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        self.runs.lock().unwrap().push(job.name.to_owned());
        self.staged.lock().unwrap().push(job.staging.clone());
        self.policies
            .lock()
            .unwrap()
            .push((job.spec.replaces_entrypoint, job.placement.time_seconds));
        tokio::time::sleep(self.delay).await;
        let mut finished = Finished {
            exit_code: Some(self.exit_code),
            files: Vec::new(),
            root: self.scratch.join(job.name),
            usage: None,
        };
        std::fs::create_dir_all(finished.root.join("work"))?;
        std::fs::create_dir_all(finished.root.join("control"))?;
        std::fs::write(finished.stdout(), "said\n")?;
        // The other containers of a task that runs several say their names and end well.
        let control = finished.root.join("control");
        std::fs::write(control.join("exit"), format!("{}\n", self.exit_code))?;
        for container in job.spec.containers.iter().skip(1) {
            let own = control.join("containers").join(&container.name);
            std::fs::create_dir_all(&own)?;
            std::fs::write(own.join("exit"), "0\n")?;
            std::fs::write(own.join("stdout"), format!("{}\n", container.name))?;
        }
        let mut input = match job.staging.inputs.first() {
            Some(file) => std::fs::read_to_string(&file.source)?,
            None => String::new(),
        };
        if self.varies {
            input.push_str(&format!("{}\n", job.name));
        }
        for rule in &job.spec.outputs {
            let path = rule.pattern.trim_end_matches('/').to_owned();
            let local = finished.local_path(&path);
            let directory = rule.pattern.ends_with('/');
            if directory {
                std::fs::create_dir_all(local.join("empty"))?;
                std::fs::write(local.join("a.txt"), &input)?;
            } else {
                std::fs::create_dir_all(local.parent().expect("a parent"))?;
                std::fs::write(&local, &input)?;
            }
            finished.files.push(Collected { path, directory });
        }
        Ok(finished)
    }
}

fn runner(root: &Path, exit_code: i32) -> Runner<Fake> {
    runner_of(root, "run1", std::time::Duration::ZERO, exit_code)
}

/// The runner of the run `run` on the store in `root`, whose tasks take `delay`.
fn runner_of(root: &Path, run: &str, delay: std::time::Duration, exit_code: i32) -> Runner<Fake> {
    runner_on(root, run, delay, exit_code, false)
}

/// The same, with a fake that writes other bytes each time when `varies` is set.
fn runner_on(
    root: &Path,
    run: &str,
    delay: std::time::Duration,
    exit_code: i32,
    varies: bool,
) -> Runner<Fake> {
    let store = Store::local(&root.join("store")).expect("a store");
    runner_with(store, root, run, delay, exit_code, varies)
}

/// The runner of the run `run` on `store`, with its scratch and downloads in `root`.
fn runner_with(
    store: Store,
    root: &Path,
    run: &str,
    delay: std::time::Duration,
    exit_code: i32,
    varies: bool,
) -> Runner<Fake> {
    let fake = Fake {
        scratch: root.join("scratch"),
        runs: Mutex::new(Vec::new()),
        staged: Mutex::new(Vec::new()),
        policies: Mutex::new(Vec::new()),
        exit_code,
        delay,
        varies,
    };
    let settings = Settings {
        downloads: root.join("downloads"),
        import_inputs: false,
        storage: storage::Storage::default(),
        digests: None,
    };
    Runner::new(store, fake, run, settings)
}

/// Task 1 of step `P`, which reads `input.txt` and writes `out.txt`.
fn task(root: &Path, command: &str) -> Task {
    let input = root.join("input.txt");
    std::fs::write(&input, "data\n").expect("writes the input");
    Task {
        id: TaskId(1),
        name: "P (1)".into(),
        step: "P".into(),
        image: Some("debian".into()),
        command: vec!["/bin/bash".into(), "-c".into(), command.into()],
        inputs: vec![Input {
            path: "input.txt".into(),
            source: input.to_string_lossy().into_owned(),
            directory: false,
            content: None,
            ..Default::default()
        }],
        outputs: vec!["out.txt".into()],
        ..Task::default()
    }
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
        .block_on(future)
}

struct ChangingPin {
    calls: std::sync::atomic::AtomicUsize,
}

impl Backend for ChangingPin {
    async fn pin(&self, image: &str) -> Result<Option<String>, ExecutorError> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if image == "conda-env:block" {
            return std::future::pending().await;
        }
        let digit = char::from_digit((call % 10) as u32, 10).unwrap();
        Ok(Some(format!("sha256:{}", digit.to_string().repeat(64))))
    }

    async fn run(
        &self,
        _job: Job<'_>,
        _cancel: &CancellationToken,
    ) -> Result<Finished, ExecutorError> {
        Err(ExecutorError::Unsupported("not run by this test".into()))
    }
}

fn pinning_runner(root: &Path) -> Runner<ChangingPin> {
    Runner::new(
        Store::memory().unwrap(),
        ChangingPin {
            calls: std::sync::atomic::AtomicUsize::new(0),
        },
        "pinning",
        Settings {
            downloads: root.join("downloads"),
            import_inputs: false,
            storage: storage::Storage::default(),
            digests: None,
        },
    )
}

#[tokio::test]
async fn refreshes_conda_pins() {
    let root = tempfile::tempdir().unwrap();
    let runner = pinning_runner(root.path());
    let marker = "conda-env:/workflow/environment.yaml";
    runner.pin_images(&[(marker.into(), "stale".into())].into());

    let first = runner.pin(marker).await.unwrap();
    let second = runner.pin(marker).await.unwrap();
    assert_ne!(first, second);
    assert_ne!(first, "stale");
    let ordinary = runner.pin("registry/image:tag").await.unwrap();
    assert_eq!(runner.pin("registry/image:tag").await.unwrap(), ordinary);
    assert_eq!(
        runner
            .backend()
            .calls
            .load(std::sync::atomic::Ordering::SeqCst),
        3
    );
}

#[tokio::test]
async fn cancels_blocked_pins() {
    use workflow_graph::{Attempt, ExecuteError, Executor};

    let root = tempfile::tempdir().unwrap();
    let runner = pinning_runner(root.path());
    let mut executor = task_runner::RunnerExecutor::new(runner, None);
    let cancel = CancellationToken::new();
    let mut running = Box::pin(executor.run(Attempt {
        task: std::sync::Arc::new(Task {
            id: TaskId(99),
            image: Some("conda-env:block".into()),
            command: vec!["true".into()],
            ..Task::default()
        }),
        number: 1,
        cancel: cancel.clone(),
    }));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), &mut running)
            .await
            .is_err()
    );
    cancel.cancel();
    let result = tokio::time::timeout(std::time::Duration::from_millis(200), &mut running)
        .await
        .expect("cancellation ends the attempt");
    assert!(matches!(result, Err(ExecuteError::Cancelled)));
    executor.settle(TaskId(99), 1, true).await.unwrap();
}

fn request(runner: &Runner<Fake>, task: &Task) -> Prepared {
    block_on(runner.request(task, Some("debian@sha256:00".into()))).expect("a request")
}

fn execute(runner: &Runner<Fake>, prepared: &Prepared) -> TaskResult {
    block_on(runner.execute(prepared, &CancellationToken::new())).expect("runs")
}

#[test]
fn tolerates_entry_failure() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().expect("a temporary directory");
    let runner = runner(root.path(), 0);
    let entries = root.path().join("store/ac/v1/default");
    std::fs::create_dir_all(&entries).unwrap();
    std::fs::set_permissions(&entries, std::fs::Permissions::from_mode(0o555)).unwrap();
    let prepared = request(&runner, &task(root.path(), "a"));
    let result = block_on(runner.execute(&prepared, &CancellationToken::new()));
    std::fs::set_permissions(&entries, std::fs::Permissions::from_mode(0o755)).unwrap();
    let result = result.expect("a failed cache write is only a warning");
    assert!(!result.reused);
    assert_eq!(std::fs::read_dir(&entries).unwrap().count(), 0);
}

#[test]
fn reuses_shared_results() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let runner = runner(root.path(), 0);
    let first = execute(&runner, &request(&runner, &task(root.path(), "a")));
    assert!(!first.reused);
    let [output] = first.files.as_slice() else {
        panic!("one output")
    };
    assert_eq!(output.location, "runs/run1/P-1/1/work/out.txt");
    let stored = runner.local_path(&output.location).expect("a local store");
    assert_eq!(std::fs::read_to_string(stored).expect("reads"), "data\n");
    let said = block_on(runner.read_content(first.stdout.as_ref().expect("a standard output")))
        .expect("reads");
    assert_eq!(said, "said\n");
    let second = execute(&runner, &request(&runner, &task(root.path(), "a")));
    assert!(second.reused);
    assert_eq!(second.files, first.files);
    assert_eq!(second.stdout, first.stdout);
    let third = execute(&runner, &request(&runner, &task(root.path(), "b")));
    assert!(!third.reused);
    assert_eq!(runner.backend().runs.lock().unwrap().len(), 2);
    // The input was linked from its private snapshot, which went with its request.
    let staged = runner.backend().staged.lock().unwrap();
    let source = &staged[0].inputs[0].source;
    assert!(source.starts_with(root.path().join("downloads/input-snapshots/run1")));
    assert!(!source.exists());
    assert!(!staged[0].inputs[0].copy);
    drop(staged);
    // Images are pinned once per run.
    let pinned = block_on(runner.pin("debian")).expect("pins");
    assert_eq!(pinned, "debian@sha256:00");
    // Each run removes its scratch directory.
    assert!(!root.path().join("scratch/run1-P-1-1").exists());
}

#[test]
fn rejects_damaged_cache() {
    for damaged in ["entry", "output", "content", "log"] {
        let root = tempfile::tempdir().unwrap();
        let runner = runner(root.path(), 0);
        let task = task(root.path(), "a");
        let prepared = request(&runner, &task);
        let key = prepared.request.spec.cache_key().unwrap();
        let namespace = &prepared.request.spec.namespace;
        let first = execute(&runner, &prepared);
        let store = Store::local(&root.path().join("store")).unwrap();
        let content_path = store
            .local_path(&content_store::layout::content(&first.files[0].digest))
            .unwrap();
        let log_path = store
            .local_path(&content_store::layout::content(
                first.stdout.as_ref().unwrap(),
            ))
            .unwrap();
        let content = std::fs::read(&content_path).unwrap();
        let log = std::fs::read(&log_path).unwrap();
        let location = match damaged {
            "entry" => content_store::layout::entry(namespace, &key).unwrap(),
            "output" => first.files[0].location.clone(),
            "content" => content_store::layout::content(&first.files[0].digest),
            _ => content_store::layout::content(first.stdout.as_ref().unwrap()),
        };
        let path = store.local_path(&location).unwrap();
        let original = std::fs::read(&path).unwrap();
        let corrupted = if damaged == "entry" {
            b"{".as_slice()
        } else {
            b"evil\n".as_slice()
        };
        overwrite(&path, corrupted);
        let lookup = task_runner::Lookup::new(
            store.clone(),
            Settings {
                downloads: root.path().join("downloads"),
                import_inputs: false,
                storage: storage::Storage::default(),
                digests: None,
            },
        );
        assert!(
            block_on(lookup.find(&task, Some("debian@sha256:00".into())))
                .unwrap()
                .is_none(),
            "{damaged}"
        );
        assert!(
            block_on(store.get_entry(namespace, &key))
                .unwrap()
                .is_some()
        );
        let fresh = execute(&runner, &prepared);
        assert!(!fresh.reused, "{damaged}");
        let replacement = block_on(store.get_entry(namespace, &key)).unwrap().unwrap();
        assert!(
            task_runner::CacheEntry::parse(&key, &replacement).is_ok(),
            "{damaged}"
        );
        if damaged != "entry" {
            overwrite(&path, &original);
        }
        overwrite(&content_path, &content);
        overwrite(&log_path, &log);
        assert!(execute(&runner, &prepared).reused, "{damaged}");
    }
}

#[test]
fn executes_copied_bytes() {
    let root = tempfile::tempdir().unwrap();
    let runner = runner(root.path(), 0);
    let task = task(root.path(), "a");
    let prepared = request(&runner, &task);
    let key = prepared.request.spec.cache_key().unwrap();
    std::fs::write(root.path().join("input.txt"), "changed\n").unwrap();
    let result = execute(&runner, &prepared);
    let output = runner.local_path(&result.files[0].location).unwrap();
    assert_eq!(std::fs::read(output).unwrap(), b"data\n");
    let snapshot = runner.backend().staged.lock().unwrap()[0].inputs[0]
        .source
        .clone();
    let again = prepared.clone();
    drop(prepared);
    assert_eq!(std::fs::read(&snapshot).unwrap(), b"data\n");
    drop(again);
    assert!(
        !snapshot.exists(),
        "the copy goes with the last clone of its request"
    );
    std::fs::write(root.path().join("input.txt"), "data\n").unwrap();
    let restored = request(&runner, &task);
    assert_eq!(restored.request.spec.cache_key().unwrap(), key);
    assert!(execute(&runner, &restored).reused);
    drop(restored);
    let snapshots = root.path().join("downloads/input-snapshots/run1");
    assert_eq!(std::fs::read_dir(snapshots).unwrap().count(), 0);
}

#[test]
fn reads_stored_inputs() {
    let root = tempfile::tempdir().unwrap();
    let runner = runner(root.path(), 0);
    let first = execute(&runner, &request(&runner, &task(root.path(), "a")));
    let stored = runner.local_path(&first.files[0].location).unwrap();
    // A later task reads the output of the first where the store keeps it, with its index.
    let mut next = task(root.path(), "b");
    std::fs::write(stored.with_extension("idx"), "index\n").unwrap();
    next.inputs[0].source = stored.to_string_lossy().into_owned();
    next.inputs.push(Input {
        path: "out.idx".into(),
        source: stored.with_extension("idx").to_string_lossy().into_owned(),
        ..Default::default()
    });
    let prepared = request(&runner, &next);
    let snapshots = root.path().join("downloads/input-snapshots/run1");
    assert_eq!(std::fs::read_dir(&snapshots).unwrap().count(), 0);
    execute(&runner, &prepared);
    let staged = runner.backend().staged.lock().unwrap();
    let source = &staged.last().unwrap().inputs[0].source;
    assert_eq!(source, &stored.canonicalize().unwrap());
    drop(staged);
    // A store that takes inputs in copies the bytes that it hashes, without a snapshot.
    let root = tempfile::tempdir().unwrap();
    let store = Store::local(&root.path().join("store")).unwrap();
    let fake = Fake {
        scratch: root.path().join("scratch"),
        runs: Mutex::default(),
        staged: Mutex::default(),
        policies: Mutex::default(),
        exit_code: 0,
        delay: std::time::Duration::ZERO,
        varies: false,
    };
    let settings = Settings {
        downloads: root.path().join("downloads"),
        import_inputs: true,
        storage: storage::Storage::default(),
        digests: None,
    };
    let imported = Runner::new(store, fake, "run2", settings);
    let prepared = request(&imported, &task(root.path(), "a"));
    assert!(!execute(&imported, &prepared).reused);
    assert!(!root.path().join("downloads/input-snapshots/run2").exists());
}

#[tokio::test]
async fn rejects_corrupt_literals() {
    for remote in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let store = if remote {
            Store::memory().unwrap()
        } else {
            Store::local(&root.path().join("store")).unwrap()
        };
        let runner = runner_with(
            store,
            root.path(),
            "literal",
            std::time::Duration::ZERO,
            0,
            false,
        );
        let mut task = task(root.path(), "literal");
        task.inputs[0].content = Some("literal\n".into());
        task.reuse = workflow_graph::Reuse::Disabled;
        let prepared = runner
            .request(&task, Some("debian@sha256:00".into()))
            .await
            .unwrap();
        runner
            .execute(&prepared, &CancellationToken::new())
            .await
            .unwrap();
        let cached = runner.backend().staged.lock().unwrap()[0].inputs[0]
            .source
            .clone();
        overwrite(&cached, b"damaged\n");
        let again = runner.execute(&prepared, &CancellationToken::new()).await;
        if remote {
            // A damaged copy of a download is downloaded again, in its own folder.
            assert!(cached.starts_with(root.path().join("downloads").join(task_runner::CONTENTS)));
            again.unwrap();
            assert_eq!(std::fs::read(&cached).unwrap(), b"literal\n");
            assert_eq!(runner.backend().runs.lock().unwrap().len(), 2);
            continue;
        }
        let error = again.unwrap_err();
        assert!(
            matches!(
                &error,
                task_runner::RunnerError::Store(content_store::StoreError::Corrupt { .. })
            ),
            "{remote}: {error}"
        );
        assert_eq!(runner.backend().runs.lock().unwrap().len(), 1);
        assert!(!cached.exists(), "the damaged content goes");
    }
}

#[test]
fn tracks_executable_identity() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let runner = runner(root.path(), 0);
    let mut task = task(root.path(), "a");
    let source = root.path().join("input.txt");
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o644)).unwrap();
    let plain = request(&runner, &task);
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();
    let executable = request(&runner, &task);
    assert_eq!(
        plain.request.spec.inputs[0].digest,
        executable.request.spec.inputs[0].digest
    );
    assert_ne!(
        plain.request.spec.cache_key().unwrap(),
        executable.request.spec.cache_key().unwrap()
    );
    assert!(!plain.request.spec.inputs[0].executable);
    assert!(executable.request.spec.inputs[0].executable);
    execute(&runner, &plain);
    execute(&runner, &executable);
    let staged = runner.backend().staged.lock().unwrap();
    assert_eq!(
        std::fs::metadata(&staged[0].inputs[0].source)
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    );
    assert!(staged[1].inputs[0].executable && staged[1].inputs[0].copy);
    drop(staged);
    task.inputs[0].writable = workflow_graph::Writable::Copy;
    assert!(!request(&runner, &task).request.spec.inputs[0].executable);
}

#[test]
fn preserves_snapshot_links() {
    let root = tempfile::tempdir().unwrap();
    let runner = runner(root.path(), 0);
    let directory = root.path().join("directory");
    for name in ["a", "b", "empty"] {
        std::fs::create_dir_all(directory.join(name)).unwrap();
    }
    std::fs::write(directory.join("a/data"), "first").unwrap();
    std::fs::write(directory.join("b/data"), "second").unwrap();
    std::os::unix::fs::symlink("a", directory.join("selected")).unwrap();
    let mut task = task(root.path(), "a");
    task.inputs.push(Input {
        path: "directory".into(),
        source: directory.to_string_lossy().into_owned(),
        directory: true,
        ..Input::default()
    });
    let first = request(&runner, &task);
    std::fs::remove_file(directory.join("selected")).unwrap();
    std::os::unix::fs::symlink("b", directory.join("selected")).unwrap();
    let second = request(&runner, &task);
    assert_ne!(
        first.request.spec.inputs[1].digest,
        second.request.spec.inputs[1].digest
    );
    execute(&runner, &first);
    let staged = runner.backend().staged.lock().unwrap();
    let snapshot = &staged[0].inputs[1].source;
    assert_eq!(
        std::fs::read_link(snapshot.join("selected")).unwrap(),
        Path::new("a")
    );
    assert_eq!(
        std::fs::read(snapshot.join("selected/data")).unwrap(),
        b"first"
    );
    assert!(snapshot.join("empty").is_dir());
    drop(staged);
    std::os::unix::fs::symlink("..", directory.join("a/cycle")).unwrap();
    let cyclic = request(&runner, &task);
    assert_ne!(
        second.request.spec.inputs[1].digest,
        cyclic.request.spec.inputs[1].digest
    );
    execute(&runner, &cyclic);
    let staged = runner.backend().staged.lock().unwrap();
    assert_eq!(
        std::fs::read_link(staged[1].inputs[1].source.join("a/cycle")).unwrap(),
        Path::new("..")
    );
    drop(staged);
    std::os::unix::fs::symlink(root.path(), directory.join("outside")).unwrap();
    let error = block_on(runner.request(&task, Some("debian@sha256:00".into()))).unwrap_err();
    assert!(error.to_string().contains("absolute or outside"), "{error}");
}

#[test]
fn preserves_stored_links() {
    use task_model::{Tree, TreeEntry};
    let root = tempfile::tempdir().unwrap();
    let runner = runner(root.path(), 0);
    let digest = block_on(runner.import_bytes(b"stored")).unwrap();
    let tree = Tree {
        entries: vec![
            TreeEntry::File {
                path: "a/data".into(),
                digest,
                size: 6,
                executable: false,
            },
            TreeEntry::Symlink {
                path: "selected".into(),
                target: "a".into(),
            },
            TreeEntry::Symlink {
                path: "a/cycle".into(),
                target: "..".into(),
            },
        ],
    };
    let tree_digest = block_on(runner.import_tree(&tree)).unwrap();
    let mut task = task(root.path(), "a");
    task.inputs.push(Input {
        path: "stored".into(),
        directory: true,
        ..Input::default()
    });
    let mut prepared = request(&runner, &task);
    prepared.request.spec.inputs[1].digest = tree_digest;
    execute(&runner, &prepared);
    let staged = runner.backend().staged.lock().unwrap();
    let directory = &staged[0].inputs[1].source;
    assert_eq!(
        std::fs::read_link(directory.join("selected")).unwrap(),
        Path::new("a")
    );
    assert_eq!(
        std::fs::read_link(directory.join("a/cycle")).unwrap(),
        Path::new("..")
    );
    assert_eq!(
        std::fs::read(directory.join("selected/data")).unwrap(),
        b"stored"
    );
}

#[tokio::test]
async fn retrieves_shared_results() {
    let operator = opendal::Operator::new(opendal::services::Memory::default()).expect("memory");
    let shared = Store::remote(operator, "s3://shared/cache").expect("a store");
    let machine = |root: &Path, run: &str| {
        let front = Store::local(&root.join("store")).expect("a folder");
        let store = Store::layered(front, shared.clone()).expect("a store in layers");
        runner_with(store, root, run, std::time::Duration::ZERO, 0, false)
    };
    let first = tempfile::tempdir().expect("a temporary directory");
    let runner = machine(first.path(), "run1");
    let prepared = runner
        .request(&task(first.path(), "a"), Some("debian@sha256:00".into()))
        .await
        .unwrap();
    let made = runner
        .execute(&prepared, &CancellationToken::new())
        .await
        .unwrap();
    assert!(!made.reused);
    let second = tempfile::tempdir().expect("a temporary directory");
    let runner = machine(second.path(), "run2");
    let prepared = runner
        .request(&task(second.path(), "a"), Some("debian@sha256:00".into()))
        .await
        .unwrap();
    let taken = runner
        .execute(&prepared, &CancellationToken::new())
        .await
        .unwrap();
    assert!(taken.reused);
    assert!(runner.backend().runs.lock().unwrap().is_empty());
    // The output is in the shared store only, until this machine reads it.
    let [output] = taken.files.as_slice() else {
        panic!("one output")
    };
    let address = format!("s3://shared/cache/{}", output.location);
    let store = Store::layered(
        Store::local(&second.path().join("store")).expect("a folder"),
        shared.clone(),
    )
    .expect("a store in layers");
    assert_eq!(store.url(&output.location), address);
    // A task of this machine that reads it takes it into the folder first.
    let mut next = task(second.path(), "b");
    next.inputs[0].source = address;
    let prepared = runner
        .request(&next, Some("debian@sha256:00".into()))
        .await
        .unwrap();
    runner
        .execute(&prepared, &CancellationToken::new())
        .await
        .unwrap();
    let kept = second.path().join("store").join(&output.location);
    assert_eq!(std::fs::read_to_string(kept).expect("reads"), "data\n");
}

#[test]
fn failures_skip_cache() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let runner = runner(root.path(), 3);
    let first = execute(&runner, &request(&runner, &task(root.path(), "a")));
    assert_eq!(first.exit_code, Some(3));
    let second = execute(&runner, &request(&runner, &task(root.path(), "a")));
    assert!(!second.reused);
}

#[test]
fn names_missing_input() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let runner = runner(root.path(), 0);
    let mut task = task(root.path(), "a");
    let missing = root.path().join("data/samples.csv");
    task.inputs[0].source = missing.to_string_lossy().into_owned();
    let Err(error) = block_on(runner.request(&task, Some("debian@sha256:00".into()))) else {
        panic!("a missing input is an error")
    };
    let text = error.to_string();
    assert_eq!(
        text,
        format!("the input {} does not exist", missing.display())
    );
}

#[test]
fn stores_directory_entry() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let runner = runner(root.path(), 0);
    let mut first = task(root.path(), "a");
    first.outputs = vec!["db/".into(), "/tmp/result.txt".into()];
    let result = execute(&runner, &request(&runner, &first));
    let [db, absolute] = result.files.as_slice() else {
        panic!("two outputs")
    };
    assert!(db.directory && !absolute.directory);
    assert_eq!(db.location, "runs/run1/P-1/1/work/db");
    assert_eq!(absolute.location, "runs/run1/P-1/1/root/tmp/result.txt");
    assert_eq!(db.size, 5);
    let on_disk = runner.local_path(&db.location).expect("a local store");
    assert_eq!(
        std::fs::read_to_string(on_disk.join("a.txt")).expect("reads"),
        "data\n"
    );
    assert!(on_disk.join("empty").is_dir());

    // The directory goes back in as an input, with its empty folder and standard input.
    let mut second = task(root.path(), "b");
    second.id = TaskId(2);
    second.inputs.push(Input {
        path: "in".into(),
        source: on_disk.to_string_lossy().into_owned(),
        directory: true,
        content: None,
        ..Default::default()
    });
    second.inputs.push(Input {
        path: "made".into(),
        source: String::new(),
        directory: true,
        content: None,
        ..Default::default()
    });
    second.stdin = Some("text\n".into());
    let prepared = request(&runner, &second);
    let spec = &prepared.request.spec;
    let kinds: Vec<FileKind> = spec.inputs.iter().map(|input| input.kind).collect();
    assert_eq!(
        kinds,
        [FileKind::File, FileKind::Directory, FileKind::Directory]
    );
    assert_eq!(spec.inputs[1].digest, db.digest);
    execute(&runner, &prepared);
    let staged = runner.backend().staged.lock().unwrap();
    let staging = staged.last().expect("a staging");
    let inputs: Vec<(&str, bool)> = staging
        .inputs
        .iter()
        .map(|input| (input.path.as_str(), input.directory))
        .collect();
    // The directory made by the task is shown as itself; the one the workflow makes is made.
    assert_eq!(inputs, [("input.txt", false), ("in", true)]);
    assert_eq!(staging.directories, ["made"]);
    let stdin = staging.stdin.as_ref().expect("a standard input");
    assert_eq!(std::fs::read_to_string(stdin).expect("reads"), "text\n");
    drop(staged);
    std::fs::remove_file(on_disk.join("a.txt")).expect("removes a descendant");
    assert!(!execute(&runner, &request(&runner, &first)).reused);
    assert!(execute(&runner, &request(&runner, &first)).reused);
}

#[test]
fn downloads_remote_once() {
    use std::io::{Read, Write};
    // A small server that answers each request with the same text, and counts them.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
    let address = listener.local_addr().expect("an address");
    let served = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = served.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut request = [0; 1024];
            let _ = stream.read(&mut request);
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let body = "data\n";
            let answer = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(answer.as_bytes());
        }
    });
    let root = tempfile::tempdir().expect("a temporary directory");
    let runner = runner(root.path(), 0);
    let mut remote = task(root.path(), "a");
    remote.inputs[0].source = format!("http://{address}/input.txt");
    for _ in 0..2 {
        let prepared = request(&runner, &remote);
        let digest = &prepared.request.spec.inputs[0].digest;
        assert_eq!(digest, &task_model::Digest::sha256(b"data\n"));
    }
    // One request for the version, and one download.
    assert_eq!(served.load(std::sync::atomic::Ordering::SeqCst), 2);
    // An address that no storage provider serves is not supported.
    remote.inputs[0].source = "az://container/input.txt".into();
    let error = block_on(runner.request(&remote, None)).unwrap_err();
    assert!(error.unsupported(), "{error}");
}

#[tokio::test]
async fn lookups_avoid_population() {
    let root = tempfile::tempdir().unwrap();
    let front = root.path().join("store");
    let store = Store::layered(Store::local(&front).unwrap(), Store::memory().unwrap()).unwrap();
    let runner = runner_with(
        store.clone(),
        root.path(),
        "preview",
        std::time::Duration::ZERO,
        0,
        false,
    );
    let mut task = task(root.path(), "preview");
    task.outputs = vec!["directory/".into()];
    let prepared = runner
        .request(&task, Some("debian@sha256:00".into()))
        .await
        .unwrap();
    runner
        .execute(&prepared, &CancellationToken::new())
        .await
        .unwrap();
    for prefix in ["cas/", "trees/"] {
        for object in store.objects(prefix).await.unwrap() {
            if !object.folder {
                std::fs::remove_file(front.join(object.location)).unwrap();
            }
        }
    }
    let before = store.objects("").await.unwrap();
    let lookup = task_runner::Lookup::new(
        store.clone(),
        Settings {
            downloads: root.path().join("downloads"),
            import_inputs: false,
            storage: storage::Storage::default(),
            digests: None,
        },
    );
    assert!(
        lookup
            .find(&task, Some("debian@sha256:00".into()))
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(store.objects("").await.unwrap(), before);
}

#[tokio::test]
async fn stages_complete_snapshots() {
    let root = tempfile::tempdir().unwrap();
    let operator = opendal::Operator::new(opendal::services::Memory::default()).unwrap();
    let shared = Store::remote(operator, "s3://shared/cache").unwrap();
    let a = shared
        .put_content(bytes::Bytes::from_static(b"a"))
        .await
        .unwrap();
    let b = shared
        .put_content(bytes::Bytes::from_static(b"b"))
        .await
        .unwrap();
    let tree = task_model::Tree {
        entries: vec![
            task_model::TreeEntry::File {
                path: "a".into(),
                digest: a,
                size: 1,
                executable: false,
            },
            task_model::TreeEntry::File {
                path: "b".into(),
                digest: b,
                size: 1,
                executable: false,
            },
            task_model::TreeEntry::EmptyDirectory {
                path: "empty".into(),
            },
        ],
    };
    let digest = shared.put_tree(&tree).await.unwrap();
    let location = "runs/earlier/task/1/work/directory";
    shared
        .put_output(
            "earlier",
            "task",
            1,
            "directory/a",
            bytes::Bytes::from_static(b"a"),
        )
        .await
        .unwrap();
    shared.put_directory(location, &digest).await.unwrap();
    let store = Store::layered(
        Store::local(&root.path().join("store")).unwrap(),
        shared.clone(),
    )
    .unwrap();
    store.local_file(&format!("{location}/a")).await.unwrap();
    let partial = store.local_path(location).unwrap();
    assert!(!partial.join("b").exists());
    let runner = runner_with(
        store.clone(),
        root.path(),
        "consumer",
        std::time::Duration::ZERO,
        0,
        false,
    );
    let mut task = task(root.path(), "directory");
    task.inputs.push(Input {
        path: "in".into(),
        source: shared.url(location),
        directory: true,
        ..Input::default()
    });
    let prepared = runner
        .request(&task, Some("debian@sha256:00".into()))
        .await
        .unwrap();
    assert_eq!(prepared.request.spec.inputs[1].digest, digest);
    runner
        .execute(&prepared, &CancellationToken::new())
        .await
        .unwrap();
    let directory = runner.backend().staged.lock().unwrap()[0].inputs[1]
        .source
        .clone();
    assert_eq!(std::fs::read(directory.join("b")).unwrap(), b"b");
    assert!(directory.join("empty").is_dir());
    let lookup = task_runner::Lookup::new(
        store.clone(),
        Settings {
            downloads: root.path().join("downloads"),
            import_inputs: false,
            storage: storage::Storage::default(),
            digests: None,
        },
    );
    for source in [
        shared.url(location),
        partial.to_string_lossy().into_owned(),
        format!("file://{}", partial.display()),
    ] {
        task.inputs[1].source = source;
        let before = store.objects("").await.unwrap();
        assert!(
            lookup
                .find(&task, Some("debian@sha256:00".into()))
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(store.objects("").await.unwrap(), before);
        let prepared = runner
            .request(&task, Some("debian@sha256:00".into()))
            .await
            .unwrap();
        assert_eq!(prepared.request.spec.inputs[1].digest, digest);
    }
    task.inputs[1].source = partial.to_string_lossy().into_owned();
    task.inputs[1].writable = workflow_graph::Writable::InPlace;
    assert_eq!(
        runner
            .request(&task, Some("debian@sha256:00".into()))
            .await
            .unwrap()
            .request
            .spec
            .inputs[1]
            .digest,
        task_model::Digest::sha256(
            std::fs::canonicalize(&partial)
                .unwrap()
                .to_string_lossy()
                .as_bytes()
        )
    );
    assert!(!partial.join("b").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn isolates_concurrent_staging() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let operator = opendal::Operator::new(opendal::services::Memory::default()).unwrap();
    let store = Store::remote(operator, "memory://staging").unwrap();
    let runner = runner_with(
        store.clone(),
        root.path(),
        "parallel",
        std::time::Duration::ZERO,
        0,
        false,
    );
    let content = "shared staged bytes\n".repeat(65_536);
    let digest = task_model::Digest::sha256(content.as_bytes());
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
    let mut runs = Vec::new();
    for index in 0..8 {
        let task = Task {
            id: TaskId(index),
            command: vec![format!("command-{index}")],
            inputs: vec![Input {
                path: "input".into(),
                content: Some(content.clone()),
                ..Input::default()
            }],
            outputs: vec!["output".into()],
            ..Task::default()
        };
        let prepared = runner
            .request(&task, Some("image@sha256:00".into()))
            .await
            .unwrap();
        let (runner, barrier) = (runner.clone(), barrier.clone());
        runs.push(tokio::spawn(async move {
            barrier.wait().await;
            runner.execute(&prepared, &CancellationToken::new()).await
        }));
    }
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        for run in runs {
            let result = run.await.unwrap().unwrap();
            assert!(!result.reused);
            assert_eq!(result.files[0].digest, digest);
        }
    })
    .await
    .unwrap();
    let contents = root.path().join("downloads").join(task_runner::CONTENTS);
    let downloaded = contents.join(digest.hex());
    assert_eq!(std::fs::read_to_string(&downloaded).unwrap(), content);
    let reference = root.path().join("ordinary-mode");
    std::fs::write(&reference, "mode").unwrap();
    assert_eq!(
        std::fs::metadata(downloaded).unwrap().permissions().mode() & 0o777,
        std::fs::metadata(reference).unwrap().permissions().mode() & 0o777
    );
    assert_eq!(std::fs::read_dir(contents).unwrap().count(), 1);
}

#[test]
fn describes_stored_files() {
    use task_runner::RunnerExecutor;
    use workflow_graph::{Attempt, Executor};
    let root = tempfile::tempdir().expect("a temporary directory");
    let mut executor = RunnerExecutor::new(runner(root.path(), 0), None);
    let attempt = Attempt {
        task: std::sync::Arc::new(task(root.path(), "a")),
        number: 2,
        cancel: CancellationToken::new(),
    };
    let outcome = block_on(executor.run(attempt)).expect("an outcome");
    assert_eq!((outcome.attempt, outcome.exit_code), (2, 0));
    assert_eq!(outcome.image.as_deref(), Some("debian@sha256:00"));
    let [file] = outcome.files.as_slice() else {
        panic!("one output")
    };
    let stored = root.path().join("store/runs/run1/P-1/2/work/out.txt");
    assert_eq!(file.location, stored.to_string_lossy());
    assert_eq!(file.size, Some(5));
    let digest = task_model::Digest::sha256(b"data\n").to_string();
    assert_eq!(file.digest.as_deref(), Some(digest.as_str()));
}

#[test]
fn preserves_group_deadlines() {
    use std::sync::Arc;
    use task_runner::RunnerExecutor;
    use workflow_graph::{Attempt, Executor};
    for (replaces, times) in [
        (false, [None, None]),
        (true, [Some(1), None]),
        (true, [Some(1), Some(2)]),
    ] {
        let root = tempfile::tempdir().expect("a temporary directory");
        let mut executor = RunnerExecutor::new(runner(root.path(), 0), None);
        executor.together = true;
        executor.measured = Arc::new(
            [(
                "P".into(),
                vec![task_model::Measured {
                    milliseconds: 1,
                    overhead: Some(100),
                    ..Default::default()
                }],
            )]
            .into(),
        );
        let attempt = |number: usize| {
            let mut task = task(root.path(), "a");
            task.id = TaskId(number as u64 + 1);
            task.replaces_entrypoint = replaces;
            task.resources.time_seconds = times[number];
            task.inputs[0].content = Some(format!("bytes-{number}\n"));
            Attempt {
                task: Arc::new(task),
                number: 1,
                cancel: CancellationToken::new(),
            }
        };
        let first = executor.run(attempt(0));
        let second = executor.run(attempt(1));
        let (first, second) = block_on(async { tokio::join!(first, second) });
        for (number, outcome) in [first, second].into_iter().enumerate() {
            let outcome = outcome.expect("an individual job succeeds");
            assert_eq!(outcome.exit_code, 0);
            assert_eq!(
                std::fs::read_to_string(&outcome.files[0].location).expect("reads"),
                format!("bytes-{number}\n")
            );
        }
        let policies = executor.runner().backend().policies.lock().unwrap();
        assert_eq!(policies.len(), 2);
        for time in times {
            assert!(policies.contains(&(replaces, time)));
        }
    }
}

#[test]
fn caches_container_logs() {
    use task_runner::RunnerExecutor;
    use workflow_graph::{Attempt, Container, Executor};
    let root = tempfile::tempdir().expect("a temporary directory");
    let mut executor = RunnerExecutor::new(runner(root.path(), 0), None);
    let mut task = task(root.path(), "a");
    let container = |name: &str, image: &str, after: &[&str]| Container {
        name: name.to_owned(),
        image: Some(image.to_owned()),
        command: task.command.clone(),
        after: after.iter().map(|name| (*name).to_owned()).collect(),
        attempts: 1,
        ..Container::default()
    };
    task.containers = vec![
        container("main", "debian", &["prepare"]),
        container("prepare", "alpine", &[]),
    ];
    let attempt = |task: &Task, number| Attempt {
        task: std::sync::Arc::new(task.clone()),
        number,
        cancel: CancellationToken::new(),
    };
    let first = block_on(executor.run(attempt(&task, 1))).expect("an outcome");
    block_on(executor.settle(first.id, first.attempt, true)).expect("accepted outputs");
    let second = block_on(executor.run(attempt(&task, 2))).expect("an outcome");
    assert_eq!((first.cached, second.cached), (false, true));
    for outcome in [&first, &second] {
        let [main, prepare] = outcome.containers.as_slice() else {
            panic!("two containers")
        };
        assert_eq!((main.name.as_str(), main.exit_code), ("main", Some(0)));
        assert_eq!(main.stdout, "said\n");
        assert_eq!(prepare.name, "prepare");
        assert_eq!(prepare.exit_code, Some(0));
        assert_eq!(prepare.stdout, "prepare\n");
    }
    // Another image of a container is another task.
    task.containers[1].image = Some("busybox".into());
    let third = block_on(executor.run(attempt(&task, 3))).expect("an outcome");
    assert!(!third.cached);
}

#[test]
fn replay_bypasses_cache() {
    use task_runner::RunnerExecutor;
    use workflow_graph::{Attempt, Executor};
    let root = tempfile::tempdir().expect("a temporary directory");
    let mut executor = RunnerExecutor::new(runner(root.path(), 0), None);
    let attempt = |number| Attempt {
        task: std::sync::Arc::new(task(root.path(), "a")),
        number,
        cancel: CancellationToken::new(),
    };
    let first = block_on(executor.run(attempt(1))).expect("an outcome");
    block_on(executor.settle(first.id, first.attempt, true)).expect("accepted outputs");
    let second = block_on(executor.run(attempt(2))).expect("an outcome");
    assert_eq!((first.cached, second.cached), (false, true));
    executor.reuse = false;
    let replayed = block_on(executor.run(attempt(3))).expect("an outcome");
    assert!(!replayed.cached, "a replay runs the task again");
}

#[test]
fn disabled_skips_cache() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let runner = runner(root.path(), 0);
    let mut disabled = task(root.path(), "a");
    disabled.reuse = workflow_graph::Reuse::Disabled;
    let prepared = request(&runner, &disabled);
    let key = prepared.request.spec.cache_key().expect("a key");
    let store = Store::local(&root.path().join("store")).expect("a store");
    assert!(!execute(&runner, &prepared).reused);
    assert!(
        block_on(store.get_entry("", &key))
            .expect("reads")
            .is_none()
    );
    let allowed = task(root.path(), "a");
    execute(&runner, &request(&runner, &allowed));
    assert!(!execute(&runner, &prepared).reused);
    let lookup = task_runner::Lookup::new(
        store,
        Settings {
            downloads: root.path().join("downloads"),
            import_inputs: false,
            storage: storage::Storage::default(),
            digests: None,
        },
    );
    assert!(
        block_on(lookup.find(&disabled, Some("debian@sha256:00".into())))
            .expect("looks")
            .is_none()
    );
    assert!(execute(&runner, &request(&runner, &allowed)).reused);
    assert_eq!(runner.backend().runs.lock().unwrap().len(), 3);
}

#[test]
fn denied_reuse_executes() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let runner = runner(root.path(), 0);
    let mut denied = task(root.path(), "a");
    denied.reuse = workflow_graph::Reuse::Denied;
    let first = execute(&runner, &request(&runner, &denied));
    let second = execute(&runner, &request(&runner, &denied));
    assert!(!first.reused && !second.reused);
    assert_eq!(runner.backend().runs.lock().unwrap().len(), 2);
    let pinned = Some("debian@sha256:00".to_owned());
    let store = Store::local(&root.path().join("store")).expect("a store");
    let settings = Settings {
        downloads: root.path().join("downloads"),
        import_inputs: false,
        storage: storage::Storage::default(),
        digests: None,
    };
    let lookup = task_runner::Lookup::new(store, settings);
    let found = block_on(lookup.find(&denied, pinned.clone())).expect("looks");
    assert!(found.is_none(), "a lookup does not answer it either");
    // The same task that allows reuse finds the stored result.
    let allowed = task(root.path(), "a");
    assert!(execute(&runner, &request(&runner, &allowed)).reused);
    let found = block_on(lookup.find(&allowed, pinned)).expect("looks");
    assert!(found.is_some());
}

#[test]
fn missing_outputs_rerun() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let runner = runner(root.path(), 0);
    let first = execute(&runner, &request(&runner, &task(root.path(), "a")));
    let stored = runner
        .local_path(&first.files[0].location)
        .expect("a local store");
    std::fs::remove_file(stored).expect("removes the output");
    let second = execute(&runner, &request(&runner, &task(root.path(), "a")));
    assert!(!second.reused);
    // The new result is stored, and answers the next request.
    let third = execute(&runner, &request(&runner, &task(root.path(), "a")));
    assert!(third.reused);
    assert_eq!(runner.backend().runs.lock().unwrap().len(), 2);
}

#[test]
fn records_entry_times() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let runner = runner_of(root.path(), "run1", std::time::Duration::from_millis(30), 0);
    let prepared = request(&runner, &task(root.path(), "a"));
    execute(&runner, &prepared);
    let key = prepared.request.spec.cache_key().expect("a key");
    let store = Store::local(&root.path().join("store")).expect("a store");
    let bytes = block_on(store.get_entry("", &key))
        .expect("reads")
        .expect("an entry");
    let entry = task_runner::CacheEntry::parse(&key, &bytes).expect("an entry");
    assert!(entry.milliseconds.expect("a time") >= 30, "{entry:?}");
    assert_eq!((entry.run.as_str(), entry.task.as_str()), ("run1", "P-1"));
    let used = root
        .path()
        .join("store")
        .join(content_store::layout::used("", &key).unwrap());
    assert!(!used.exists());
    execute(&runner, &prepared);
    assert_eq!(std::fs::read_to_string(used).expect("a mark"), "run1");
}

#[test]
fn coalesces_concurrent_tasks() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let delay = std::time::Duration::from_millis(300);
    // Two runs on one store, and two tasks of the first run, ask for the same task at once.
    let first = runner_of(root.path(), "run1", delay, 0);
    let second = runner_of(root.path(), "run2", delay, 0);
    let prepared = request(&first, &task(root.path(), "a"));
    let cancel = CancellationToken::new();
    let (a, b, c) = block_on(async {
        tokio::join!(
            first.execute(&prepared, &cancel),
            first.execute(&prepared, &cancel),
            second.execute(&prepared, &cancel),
        )
    });
    let results = [a.expect("runs"), b.expect("runs"), c.expect("runs")];
    let ran =
        first.backend().runs.lock().unwrap().len() + second.backend().runs.lock().unwrap().len();
    assert_eq!(ran, 1);
    assert_eq!(results.iter().filter(|result| result.reused).count(), 2);
    assert!(
        results
            .iter()
            .all(|result| result.files == results[0].files)
    );
}

#[test]
fn failure_releases_turn() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let delay = std::time::Duration::from_millis(100);
    let failing = runner_of(root.path(), "run1", delay, 3);
    let working = runner_of(root.path(), "run2", delay, 0);
    let prepared = request(&failing, &task(root.path(), "a"));
    let cancel = CancellationToken::new();
    let (failed, worked) = block_on(async {
        tokio::join!(failing.execute(&prepared, &cancel), async {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            working.execute(&prepared, &cancel).await
        })
    });
    assert_eq!(failed.expect("runs").exit_code, Some(3));
    let worked = worked.expect("runs");
    assert_eq!((worked.exit_code, worked.reused), (Some(0), false));
}

#[test]
fn crash_releases_task() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let runner = runner(root.path(), 0);
    let prepared = request(&runner, &task(root.path(), "a"));
    let key = prepared.request.spec.cache_key().expect("a key");
    // Another process holds the lease of the key, as a run that runs the task does, and is
    // killed. Without forking, the killed process is the one that holds the lock.
    let lock = root
        .path()
        .join("store/locks/tasks/default")
        .join(key.hex());
    std::fs::create_dir_all(lock.parent().unwrap()).expect("makes the folder");
    let mut holder = std::process::Command::new("flock")
        .arg("--no-fork")
        .arg(&lock)
        .args(["sleep", "60"])
        .spawn()
        .expect("runs flock");
    let store = Store::local(&root.path().join("store")).expect("a store");
    while block_on(store.held_leases("tasks/"))
        .expect("lists")
        .is_empty()
    {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let begun = std::time::Instant::now();
    let killer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(500));
        holder.kill().expect("kills the holder");
        holder.wait().expect("waits for it");
    });
    let result = execute(&runner, &prepared);
    killer.join().expect("the holder was killed");
    assert!(!result.reused);
    let waited = begun.elapsed();
    assert!(
        waited >= std::time::Duration::from_millis(500),
        "{waited:?}"
    );
    assert!(waited < std::time::Duration::from_secs(5), "{waited:?}");
    assert_eq!(runner.backend().runs.lock().unwrap().len(), 1);
}

#[test]
fn cancels_waiting_run() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let delay = std::time::Duration::from_secs(30);
    let slow = runner_of(root.path(), "run1", delay, 0);
    let waiting = runner_of(root.path(), "run2", delay, 0);
    let prepared = request(&slow, &task(root.path(), "a"));
    let cancel = CancellationToken::new();
    let stop = CancellationToken::new();
    let result = block_on(async {
        tokio::select! {
            _ = slow.execute(&prepared, &cancel) => None,
            waited = async {
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        let started = { !slow.backend().runs.lock().unwrap().is_empty() };
                        if started {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                }).await.expect("the slow backend starts while holding the task lease");
                let stopper = stop.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    stopper.cancel();
                });
                waiting.execute(&prepared, &stop).await
            } => Some(waited),
        }
    });
    let error = result.expect("the waiting task ends first").unwrap_err();
    assert!(error.to_string().contains("cancelled"), "{error}");
}

#[test]
fn lookup_preserves_store() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let runner = runner(root.path(), 0);
    let mut made = task(root.path(), "a");
    made.stdin = Some("text\n".into());
    execute(&runner, &request(&runner, &made));
    let store = Store::local(&root.path().join("store")).expect("a store");
    let settings = Settings {
        downloads: root.path().join("downloads"),
        import_inputs: false,
        storage: storage::Storage::default(),
        digests: None,
    };
    let lookup = task_runner::Lookup::new(store.clone(), settings);
    let objects = || block_on(store.objects("")).expect("lists").len();
    let before = objects();
    let pinned = Some("debian@sha256:00".to_owned());
    let found = block_on(lookup.find(&made, pinned.clone())).expect("looks");
    let found = found.expect("a stored result");
    assert!(found.cached);
    assert_eq!(found.stdout, "said\n");
    let stored = root.path().join("store/runs/run1/P-1/1/work/out.txt");
    assert_eq!(found.files[0].location, stored.to_string_lossy());
    // Another script, an image without a digest and a missing input are no hits.
    let other = task(root.path(), "b");
    assert!(
        block_on(lookup.find(&other, pinned.clone()))
            .expect("looks")
            .is_none()
    );
    let tagged = Some("debian".to_owned());
    assert!(
        block_on(lookup.find(&made, tagged))
            .expect("looks")
            .is_none()
    );
    let mut missing = made.clone();
    missing.inputs[0].source = root.path().join("gone.txt").to_string_lossy().into_owned();
    assert!(
        block_on(lookup.find(&missing, pinned))
            .expect("looks")
            .is_none()
    );
    assert_eq!(objects(), before);
}

/// A task like [`task`], whose input holds `text`, so that its output does too.
fn task_with(root: &Path, id: u64, text: &str) -> Task {
    let input = root.join(format!("input-{id}.txt"));
    std::fs::write(&input, text).expect("writes the input");
    let mut made = task(root, "a");
    made.id = TaskId(id);
    made.inputs[0].source = input.to_string_lossy().into_owned();
    made
}

#[test]
fn retains_named_runs() {
    use task_runner::clean::{Keep, plan, remove};
    let root = tempfile::tempdir().expect("a temporary directory");
    let old = runner_of(root.path(), "old", std::time::Duration::ZERO, 0);
    let kept = runner_of(root.path(), "kept", std::time::Duration::ZERO, 0);
    let first = task_with(root.path(), 1, "first\n");
    let second = task_with(root.path(), 2, "second\n");
    let third = task_with(root.path(), 3, "third\n");
    execute(&old, &request(&old, &first));
    let taken = execute(&old, &request(&old, &third));
    execute(&kept, &request(&kept, &second));
    // The kept run took the third result from the cache: it refers to files of the old run.
    let reused = execute(&kept, &request(&kept, &third));
    assert!(reused.reused);
    drop(old);
    drop(kept);
    let store = Store::local(&root.path().join("store")).expect("a store");
    let keep = Keep {
        runs: ["kept".to_owned()].into(),
        locations: taken
            .files
            .iter()
            .map(|file| file.location.clone())
            .collect(),
        ..Keep::default()
    };
    let planned = block_on(plan(&store, &keep)).expect("plans");
    // Only the result of the first task goes: its entry, its output and its content.
    assert_eq!(planned.results.len(), 1);
    assert_eq!(planned.results[0].task.as_deref(), Some("P-1"));
    let mut locations: Vec<&str> = (planned.objects.iter())
        .map(|object| object.location.as_str())
        .collect();
    locations.sort_unstable();
    let content = content_store::layout::content(&task_model::Digest::sha256(b"first\n"));
    assert_eq!(locations, [content.as_str(), "runs/old/P-1/1/work/out.txt"]);
    // The output and its content are one file with two names: its space counts once.
    assert_eq!(planned.freed(), 6);
    assert_eq!((planned.stored_results, planned.stored_objects), (3, 7));
    let removed = block_on(remove(&store, &planned)).expect("removes");
    assert_eq!((removed.results, removed.objects), (1, 2));
    let kept = runner_of(root.path(), "kept", std::time::Duration::ZERO, 0);
    // What the kept run needs is still answered from the cache; the first task runs again.
    assert!(execute(&kept, &request(&kept, &third)).reused);
    assert!(execute(&kept, &request(&kept, &second)).reused);
    assert!(!execute(&kept, &request(&kept, &first)).reused);
}

#[test]
fn retains_used_slow() {
    use task_runner::clean::{Keep, plan};
    let root = tempfile::tempdir().expect("a temporary directory");
    let quick = runner_of(root.path(), "quick", std::time::Duration::ZERO, 0);
    let slow = runner_of(
        root.path(),
        "slow",
        std::time::Duration::from_millis(200),
        0,
    );
    let first = task_with(root.path(), 1, "first\n");
    let second = task_with(root.path(), 2, "second\n");
    let third = task_with(root.path(), 3, "third\n");
    execute(&quick, &request(&quick, &first));
    execute(&quick, &request(&quick, &second));
    execute(&slow, &request(&slow, &third));
    std::thread::sleep(std::time::Duration::from_millis(20));
    let lately = std::time::SystemTime::now();
    // A later run uses the second result.
    assert!(execute(&quick, &request(&quick, &second)).reused);
    drop(quick);
    drop(slow);
    let store = Store::local(&root.path().join("store")).expect("a store");
    let tasks = |keep: &Keep| -> Vec<String> {
        let planned = block_on(plan(&store, keep)).expect("plans");
        let mut tasks: Vec<String> = (planned.results.iter())
            .filter_map(|result| result.task.clone())
            .collect();
        tasks.sort();
        tasks
    };
    let used = Keep {
        used_since: Some(lately),
        ..Keep::default()
    };
    assert_eq!(tasks(&used), ["P-1", "P-3"]);
    let long = Keep {
        slower_than: Some(std::time::Duration::from_millis(150)),
        ..Keep::default()
    };
    assert_eq!(tasks(&long), ["P-1", "P-2"]);
    // What is newer than the oldest run that goes on stays, whatever the rules say.
    let active = Keep {
        active_since: Some(std::time::SystemTime::UNIX_EPOCH),
        ..Keep::default()
    };
    let planned = block_on(plan(&store, &active)).expect("plans");
    assert!(planned.is_empty());
}

#[test]
fn rejects_stale_cleanup() {
    use task_runner::clean::{Keep, plan, remove};
    let root = tempfile::tempdir().unwrap();
    let task = task(root.path(), "a");
    let old = runner_of(root.path(), "old", std::time::Duration::ZERO, 0);
    let result = execute(&old, &request(&old, &task));
    drop(old);
    let store = Store::local(&root.path().join("store")).unwrap();
    let stale = block_on(plan(&store, &Keep::default())).unwrap();
    assert!(!stale.objects.is_empty());
    let consumer = runner_of(root.path(), "consumer", std::time::Duration::ZERO, 0);
    assert!(execute(&consumer, &request(&consumer, &task)).reused);
    let removed = block_on(remove(&store, &stale)).unwrap();
    assert_eq!((removed.results, removed.objects), (0, 0));
    assert_eq!(
        block_on(store.get_object(&result.files[0].location)).unwrap(),
        b"data\n".as_slice()
    );
    drop(consumer);
    assert!(block_on(remove(&store, &stale)).unwrap().objects > 0);
}

#[tokio::test]
async fn maintenance_blocks_preparation() {
    use task_runner::clean::{Keep, maintenance, plan, remove_guarded};
    let root = tempfile::tempdir().unwrap();
    let task = task(root.path(), "a");
    let old = runner_of(root.path(), "old", std::time::Duration::ZERO, 0);
    let prepared = old
        .request(&task, Some("debian@sha256:00".into()))
        .await
        .unwrap();
    let result = old
        .execute(&prepared, &CancellationToken::new())
        .await
        .unwrap();
    drop(old);
    let store = Store::local(&root.path().join("store")).unwrap();
    let guard = maintenance(&store).await.unwrap();
    let planned = plan(&store, &Keep::default()).await.unwrap();
    let consumer = std::sync::Arc::new(runner_of(
        root.path(),
        "consumer",
        std::time::Duration::ZERO,
        0,
    ));
    let pending = consumer.clone();
    let mut preparing = tokio::spawn(async move {
        pending
            .request(&task, Some("debian@sha256:00".into()))
            .await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), &mut preparing)
            .await
            .is_err()
    );
    assert!(store.held_leases("runs/").await.unwrap().is_empty());
    remove_guarded(&store, &planned, &guard).await.unwrap();
    assert!(!store.exists(&result.files[0].location).await.unwrap());
    drop(guard);
    let prepared = preparing.await.unwrap().unwrap();
    assert!(
        !consumer
            .execute(&prepared, &CancellationToken::new())
            .await
            .unwrap()
            .reused
    );
}

#[test]
fn retries_run_lease() {
    let root = tempfile::tempdir().unwrap();
    let runner = runner(root.path(), 0);
    let task = task(root.path(), "a");
    let blocker = root.path().join("store/locks/runs");
    std::fs::create_dir_all(blocker.parent().unwrap()).unwrap();
    std::fs::write(&blocker, "blocked").unwrap();
    assert!(block_on(runner.request(&task, Some("debian@sha256:00".into()))).is_err());
    assert!(!root.path().join("downloads/input-snapshots").exists());
    assert!(runner.backend().runs.lock().unwrap().is_empty());
    std::fs::remove_file(blocker).unwrap();
    let prepared = request(&runner, &task);
    assert!(!execute(&runner, &prepared).reused);
}

#[tokio::test]
async fn cancels_maintenance_wait() {
    let root = tempfile::tempdir().unwrap();
    let task = task(root.path(), "a");
    let original = runner_of(root.path(), "original", std::time::Duration::ZERO, 0);
    let prepared = original
        .request(&task, Some("debian@sha256:00".into()))
        .await
        .unwrap();
    drop(original);
    let store = Store::local(&root.path().join("store")).unwrap();
    let _guard = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            match task_runner::clean::maintenance(&store).await {
                Ok(guard) => break guard,
                Err(task_runner::RunnerError::Io(error))
                    if error.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
                Err(error) => panic!("{error}"),
            }
        }
    })
    .await
    .expect("the preceding preparation must release maintenance");
    let consumer = runner_of(root.path(), "consumer", std::time::Duration::ZERO, 0);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let error = consumer.execute(&prepared, &cancel).await.unwrap_err();
    assert!(matches!(
        error,
        task_runner::RunnerError::Executor(ExecutorError::Cancelled)
    ));
    assert!(store.held_leases("runs/").await.unwrap().is_empty());
    assert!(consumer.backend().runs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cancels_blocked_preparation() {
    use workflow_graph::{Attempt, ExecuteError, Executor};
    let root = tempfile::tempdir().unwrap();
    let task = std::sync::Arc::new(task(root.path(), "a"));
    let mut executor = task_runner::RunnerExecutor::new(runner(root.path(), 0), None);
    let store = Store::local(&root.path().join("store")).unwrap();
    let guard = task_runner::clean::maintenance(&store).await.unwrap();
    let cancel = CancellationToken::new();
    let mut running = Box::pin(executor.run(Attempt {
        task,
        number: 1,
        cancel: cancel.clone(),
    }));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), &mut running)
            .await
            .is_err()
    );
    cancel.cancel();
    let result = tokio::time::timeout(std::time::Duration::from_millis(200), &mut running)
        .await
        .unwrap();
    assert!(matches!(result, Err(ExecuteError::Cancelled)));
    drop(guard);
    tokio::task::yield_now().await;
    assert!(!root.path().join("downloads/input-snapshots").exists());
    assert!(store.held_leases("runs/").await.unwrap().is_empty());
    assert!(executor.runner().backend().runs.lock().unwrap().is_empty());
}

#[test]
fn disables_unstable_sharing() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let zero = std::time::Duration::ZERO;
    let made = task(root.path(), "a");
    let first = runner_of(root.path(), "run1", zero, 0);
    execute(&first, &request(&first, &made));
    // Every result from the cache runs again; this tool gives the same bytes.
    let mut second = runner_of(root.path(), "run2", zero, 0);
    second.check_reproducibility(1.0);
    let checked = execute(&second, &request(&second, &made));
    assert!(checked.reused);
    let check = checked.check.expect("a check");
    assert_eq!(check.earlier_run, "run1");
    assert_eq!(check.verdict, workflow_graph::Verdict::Same);
    assert_eq!(second.backend().runs.lock().unwrap().len(), 1);
    // Without the check, the result comes from the cache and the task does not run.
    let third = runner_of(root.path(), "run3", zero, 0);
    assert!(execute(&third, &request(&third, &made)).reused);
    assert!(third.backend().runs.lock().unwrap().is_empty());
    // A tool that writes other bytes each time is marked and shares no results any more.
    let mut varying = runner_on(root.path(), "run4", zero, 0, true);
    varying.check_reproducibility(1.0);
    let checked = execute(&varying, &request(&varying, &made));
    assert!(checked.reused, "the run keeps the result of the cache");
    let expected = workflow_graph::Verdict::Differs {
        paths: vec!["out.txt".into()],
    };
    assert_eq!(checked.check.expect("a check").verdict, expected);
    let later = runner_of(root.path(), "run5", zero, 0);
    let result = execute(&later, &request(&later, &made));
    assert!(!result.reused, "the tool is marked");
    assert_eq!(later.backend().runs.lock().unwrap().len(), 1);
    let marks = std::fs::read_dir(root.path().join("store/tools")).expect("marks");
    assert_eq!(marks.count(), 1);
    let store = Store::local(&root.path().join("store")).expect("a store");
    let lookup = task_runner::Lookup::new(
        store,
        Settings {
            downloads: root.path().join("downloads"),
            import_inputs: false,
            storage: storage::Storage::default(),
            digests: None,
        },
    );
    assert!(
        block_on(lookup.find(&made, Some("debian@sha256:00".into())))
            .expect("looks")
            .is_none()
    );
    // Another step with the same image is another tool.
    let mut other = task(root.path(), "a");
    other.step = "Q".into();
    let result = execute(&later, &request(&later, &other));
    assert!(
        result.reused,
        "the same script of another step shares its key"
    );
}

#[test]
fn changed_inputs_uncached() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let runner = runner(root.path(), 0);
    let project = root.path().join("project");
    std::fs::create_dir_all(&project).expect("makes the folder");
    std::fs::write(project.join("big.dat"), "data\n").expect("writes");
    let mut task = task(root.path(), "a");
    task.inputs.push(Input {
        path: ".".into(),
        source: project.to_string_lossy().into_owned(),
        directory: true,
        writable: workflow_graph::Writable::InPlace,
        ..Input::default()
    });
    let prepared = request(&runner, &task);
    assert_eq!(prepared.request.scope, task_model::ReuseScope::None);
    let input = &prepared.request.spec.inputs[1];
    assert!(input.writable);
    let place = project.canonicalize().expect("a path");
    let digest = task_model::Digest::sha256(place.to_string_lossy().as_bytes());
    assert_eq!(input.digest, digest);
    let result = execute(&runner, &prepared);
    assert!(!result.reused);
    {
        let staged = runner.backend().staged.lock().unwrap();
        let staged = &staged.last().expect("staged").inputs[1];
        assert!(staged.in_place);
        assert_eq!(staged.source, place);
    }
    // A copy of its own is what a writable input gets otherwise.
    task.inputs[1].writable = workflow_graph::Writable::Copy;
    task.inputs[1].path = "project".into();
    let prepared = request(&runner, &task);
    assert!(prepared.request.spec.inputs[1].writable);
    assert_eq!(prepared.request.scope, task_model::ReuseScope::Shared);
}

#[test]
fn pins_given_digests() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let runner = runner(root.path(), 0);
    let given = [("debian".to_owned(), "debian@sha256:11".to_owned())].into();
    runner.pin_images(&given);
    assert_eq!(
        block_on(runner.pin("debian")).expect("pins"),
        "debian@sha256:11"
    );
    // Other images are pinned by the backend, as before.
    assert_eq!(
        block_on(runner.pin("alpine")).expect("pins"),
        "alpine@sha256:00"
    );
}

/// Changes a file of the store the way a user can: unlock it, then write other bytes.
fn overwrite(path: &std::path::Path, bytes: &[u8]) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644));
    std::fs::write(path, bytes).unwrap();
}
