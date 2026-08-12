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
fn a_result_of_another_machine_comes_through_the_shared_store() {
    let operator = opendal::Operator::new(opendal::services::Memory::default()).expect("memory");
    let shared = Store::remote(operator, "s3://shared/cache").expect("a store");
    let machine = |root: &Path, run: &str| {
        let front = Store::local(&root.join("store")).expect("a folder");
        let store = Store::layered(front, shared.clone()).expect("a store in layers");
        runner_with(store, root, run, std::time::Duration::ZERO, 0, false)
    };
    let first = tempfile::tempdir().expect("a temporary directory");
    let runner = machine(first.path(), "run1");
    let made = execute(&runner, &request(&runner, &task(first.path(), "a")));
    assert!(!made.reused);
    let second = tempfile::tempdir().expect("a temporary directory");
    let runner = machine(second.path(), "run2");
    let taken = execute(&runner, &request(&runner, &task(second.path(), "a")));
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
    let prepared = request(&runner, &next);
    execute(&runner, &prepared);
    let kept = second.path().join("store").join(&output.location);
    assert_eq!(std::fs::read_to_string(kept).expect("reads"), "data\n");
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
fn a_missing_input_names_its_file() {
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
fn containers_of_a_task_are_pinned_and_keep_their_logs_in_the_cache() {
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
fn a_replay_runs_its_task_whatever_the_cache_holds() {
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
    let second = block_on(executor.run(attempt(2))).expect("an outcome");
    assert_eq!((first.cached, second.cached), (false, true));
    executor.reuse = false;
    let replayed = block_on(executor.run(attempt(3))).expect("an outcome");
    assert!(!replayed.cached, "a replay runs the task again");
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
fn clean_up_keeps_what_the_named_runs_need() {
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
    // What the kept run needs is still answered from the cache; the first task runs again.
    assert!(execute(&kept, &request(&kept, &third)).reused);
    assert!(execute(&kept, &request(&kept, &second)).reused);
    assert!(!execute(&kept, &request(&kept, &first)).reused);
}

#[test]
fn clean_up_keeps_what_was_used_lately_and_what_took_long() {
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
fn the_reproducibility_check_keeps_tools_that_give_other_bytes_from_sharing() {
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
fn inputs_changed_in_place_are_named_by_their_path_and_never_cached() {
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
fn images_can_be_pinned_to_given_digests() {
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
