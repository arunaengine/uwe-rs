//! Checks the cache, the staging and the stored outputs of the runner with a fake executor.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

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
    exit_code: i32,
    /// How long each task takes.
    delay: std::time::Duration,
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
        let input = match job.staging.inputs.first() {
            Some(file) => std::fs::read_to_string(&file.source)?,
            None => String::new(),
        };
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
            finished.files.push(Collected {
                path,
                directory,
                size: if directory { 0 } else { input.len() as u64 },
            });
        }
        Ok(finished)
    }
}

fn runner(root: &Path, exit_code: i32) -> Runner<Fake> {
    runner_of(root, "run1", std::time::Duration::ZERO, exit_code)
}

/// The runner of the run `run` on the store in `root`, whose tasks take `delay`.
fn runner_of(root: &Path, run: &str, delay: std::time::Duration, exit_code: i32) -> Runner<Fake> {
    let store = Store::local(&root.join("store")).expect("a store");
    let fake = Fake {
        scratch: root.join("scratch"),
        runs: Mutex::new(Vec::new()),
        staged: Mutex::new(Vec::new()),
        exit_code,
        delay,
    };
    let settings = Settings {
        downloads: root.join("downloads"),
        import_inputs: false,
        storage: storage::Storage::default(),
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

fn request(runner: &Runner<Fake>, task: &Task) -> Prepared {
    block_on(runner.request(task, Some("debian@sha256:00".into()))).expect("a request")
}

fn execute(runner: &Runner<Fake>, prepared: &Prepared) -> TaskResult {
    block_on(runner.execute(prepared, &CancellationToken::new())).expect("runs")
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
    // The input was linked where it is, not copied into the store.
    let staged = runner.backend().staged.lock().unwrap();
    let source = &staged[0].inputs[0].source;
    assert_eq!(
        source,
        &root.path().canonicalize().unwrap().join("input.txt")
    );
    assert!(!staged[0].inputs[0].copy);
    drop(staged);
    // Images are pinned once per run.
    let pinned = block_on(runner.pin("debian")).expect("pins");
    assert_eq!(pinned, "debian@sha256:00");
    // Each run removes its scratch directory.
    assert!(!root.path().join("scratch/run1-P-1-1").exists());
}

#[test]
fn failures_are_not_cached() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let runner = runner(root.path(), 3);
    let first = execute(&runner, &request(&runner, &task(root.path(), "a")));
    assert_eq!(first.exit_code, Some(3));
    let second = execute(&runner, &request(&runner, &task(root.path(), "a")));
    assert!(!second.reused);
}

#[test]
fn directories_travel_as_one_entry() {
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
    });
    second.inputs.push(Input {
        path: "made".into(),
        source: String::new(),
        directory: true,
        content: None,
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
}

#[test]
fn remote_inputs_are_downloaded_once() {
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
    assert_eq!(served.load(std::sync::atomic::Ordering::SeqCst), 1);
    // An address that no storage provider serves is not supported.
    remote.inputs[0].source = "az://container/input.txt".into();
    let error = block_on(runner.request(&remote, None)).unwrap_err();
    assert!(error.unsupported(), "{error}");
}

#[test]
fn outcomes_give_the_stored_files_with_size_and_digest() {
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
fn results_whose_files_are_gone_run_again() {
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
fn entries_say_how_long_the_task_ran_and_when_it_was_used() {
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
fn the_same_task_at_the_same_moment_runs_once() {
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
fn a_task_that_fails_leaves_its_turn_to_the_next() {
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
fn a_crashed_holder_frees_its_task_at_once() {
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
fn waiting_stops_when_the_run_is_cancelled() {
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
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
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
fn a_lookup_finds_stored_results_without_writing_anything() {
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
