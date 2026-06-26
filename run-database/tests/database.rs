//! Records small runs and reads them back: runs, tasks, attempts, files, edges, logs, events,
//! resume lookups and the schema version.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use run_database::{Database, DatabaseError, LOG_LIMIT, RunStart, VERSION};
use workflow_graph::{Edge, EdgeKind, Input, Language, Outcome, OutputFile, Task, TaskId};

fn start(id: &str, resumes: Option<&str>) -> RunStart {
    RunStart {
        id: id.into(),
        resumes: resumes.map(str::to_owned),
        workflow: "/work/main.nf".into(),
        language: Language::Nextflow,
        revision: Some("0123abc".into()),
        arguments: vec!["run".into(), "main.nf".into()],
        parameters: vec![("outdir".into(), "results".into())],
        executor: "simulator".into(),
        simulated: true,
        store: None,
    }
}

fn task(id: u64, step: &str, inputs: Vec<Input>) -> Task {
    Task {
        id: TaskId(id),
        name: format!("{step} ({id})"),
        step: step.into(),
        image: Some("debian:stable-slim".into()),
        command: vec!["bash".into(), "-ue".into(), ".command.sh".into()],
        inputs,
        outputs: vec!["out.txt".into()],
        ..Task::default()
    }
}

fn input(source: &Path) -> Input {
    Input {
        path: "in.txt".into(),
        source: source.to_string_lossy().into_owned(),
        directory: false,
        content: None,
    }
}

fn outcome(id: u64, location: PathBuf, stdout: &str) -> Outcome {
    Outcome {
        id: TaskId(id),
        exit_code: 0,
        files: vec![OutputFile {
            path: "out.txt".into(),
            location,
            directory: false,
        }],
        stdout: stdout.into(),
        stderr: String::new(),
    }
}

/// Records a run of two tasks, where the second reads the output of the first and fails.
fn record(path: &Path, directory: &Path) -> Result<(), DatabaseError> {
    let data = directory.join("data.txt");
    std::fs::write(&data, "raw").expect("writes the data");
    let made = directory.join("made.txt");
    std::fs::write(&made, "made").expect("writes an output");
    let mut recorder = Database::open(path)?.record(&start("run-1", None))?;
    let first = task(1, "MAKE", vec![input(&data)]);
    recorder.requested(&first, &[]);
    recorder.started(TaskId(1), 1);
    recorder.finished(&outcome(1, made.clone(), "hello\n"), 1, false);
    recorder.reused(TaskId(1), 1, "run-0");
    let second = task(2, "USE", vec![input(&made)]);
    let edge = Edge {
        from: TaskId(1),
        to: TaskId(2),
        kind: EdgeKind::File {
            path: "in.txt".into(),
        },
    };
    recorder.requested(&second, &[edge]);
    recorder.started(TaskId(2), 1);
    let failed = Outcome {
        exit_code: 1,
        stderr: format!("{}\nlast line", "x".repeat(LOG_LIMIT)),
        ..outcome(2, directory.join("never.txt"), "")
    };
    recorder.finished(&failed, 1, false);
    recorder.marked(
        TaskId(1),
        "content",
        "the workflow read made.txt while it planned",
    );
    recorder.requested(&task(3, "LATER", Vec::new()), &[]);
    recorder.ended(Some("task USE (2) failed"));
    recorder.close()
}

#[test]
fn a_run_is_read_back_with_its_logs_and_events() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("cache/runs.sqlite");
    record(&path, directory.path()).expect("records the run");
    let database = Database::open(&path).expect("opens again");

    let runs = database.runs(10).expect("lists runs");
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!((run.id.as_str(), run.lineage.as_str()), ("run-1", "run-1"));
    assert_eq!(run.state, "failed");
    assert_eq!(run.error.as_deref(), Some("task USE (2) failed"));
    assert!(run.simulated);
    assert_eq!(
        run.parameters,
        [("outdir".to_owned(), "results".to_owned())]
    );
    assert_eq!(run.arguments, ["run", "main.nf"]);
    let counts: Vec<(&str, u64)> = run.tasks.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    assert_eq!(counts, [("failed", 1), ("stopped", 1), ("succeeded", 1)]);

    let log = database.log("run-1", 1, 1, "stdout").expect("reads");
    let log = log.expect("a log");
    assert_eq!((log.text.as_str(), log.whole), ("hello\n", true));
    let log = database.log("run-1", 2, 1, "stderr").expect("reads");
    let log = log.expect("a log");
    assert!(!log.whole && log.text.ends_with("last line"));
    assert_eq!(log.size as usize, LOG_LIMIT + "\nlast line".len());
    assert_eq!(database.log("run-1", 1, 1, "stderr").expect("reads"), None);

    let events = database.events("run-1", 0, 100).expect("lists events");
    let kinds: Vec<&str> = events.iter().map(|event| event.kind.as_str()).collect();
    let expected = [
        "requested",
        "started",
        "finished",
        "reused",
        "requested",
        "started",
        "finished",
        "marked",
        "requested",
        "ended",
    ];
    assert_eq!(kinds, expected);
    assert_eq!(database.events("run-1", 9, 100).expect("lists").len(), 1);
    assert_eq!(database.last_event("run-1").expect("reads"), 10);
    assert_eq!(database.last_event("run-9").expect("reads"), 0);
}

#[test]
fn tasks_are_read_back_with_their_files_and_edges() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("runs.sqlite");
    record(&path, directory.path()).expect("records the run");
    let database = Database::open(&path).expect("opens again");

    let tasks = database.tasks("run-1").expect("lists tasks");
    let states: Vec<(u64, &str, Option<i32>)> = tasks
        .iter()
        .map(|task| (task.id, task.state.as_str(), task.exit_code))
        .collect();
    assert_eq!(
        states,
        [
            (1, "succeeded", Some(0)),
            (2, "failed", Some(1)),
            (3, "stopped", None)
        ]
    );

    let detail = database.task("run-1", 2).expect("reads").expect("a task");
    assert_eq!(detail.description.step, "USE");
    assert_eq!(detail.attempts.len(), 1);
    assert_eq!(detail.inputs.len(), 1);
    assert_eq!(detail.inputs[0].size, Some(4));
    assert_eq!(detail.outputs.len(), 1);
    // The output of the failed task does not exist, so its size is unknown.
    assert_eq!(detail.outputs[0].size, None);

    let markers = database.markers("run-1").expect("lists markers");
    assert_eq!(markers.len(), 1);
    assert_eq!((markers[0].task, markers[0].kind.as_str()), (1, "content"));

    let edges = database.edges("run-1").expect("lists edges");
    assert_eq!((edges[0].source, edges[0].target), (1, 2));
    assert_eq!(edges[0].path.as_deref(), Some("in.txt"));

    let made = directory.path().join("made.txt");
    let producers = database.producers(&made.to_string_lossy()).expect("finds");
    assert_eq!(producers.len(), 1);
    assert_eq!((producers[0].0.as_str(), producers[0].1.task), ("run-1", 1));
}

#[test]
fn a_resumed_run_joins_the_lineage_and_finds_finished_tasks() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("runs.sqlite");
    record(&path, directory.path()).expect("records the run");
    let database = Database::open(&path).expect("opens again");
    let last = database.last_run("/work/main.nf", false).expect("finds");
    assert_eq!(last.map(|run| run.id), Some("run-1".to_owned()));
    // The run was simulated, so there is no real run to resume.
    assert_eq!(
        database.last_run("/work/main.nf", true).expect("finds"),
        None
    );

    let reusable = database.reusable(&["run-1".to_owned()]).expect("finds");
    // Only the first task succeeded.
    assert_eq!(reusable.len(), 1);
    let (fingerprint, found) = reusable.iter().next().expect("one task");
    assert_eq!(found.outcome.stdout, "hello\n");
    assert_eq!(found.outcome.files[0].path, "out.txt");
    let detail = database.task("run-1", 1).expect("reads").expect("a task");
    assert_eq!(&detail.fingerprint, fingerprint);

    let recorder = database
        .record(&start("run-2", Some("run-1")))
        .expect("starts");
    recorder.close().expect("closes");
    let database = Database::open(&path).expect("opens again");
    let run = database.run("run-2").expect("reads").expect("a run");
    assert_eq!(
        (run.lineage.as_str(), run.resumes.as_deref()),
        ("run-1", Some("run-1"))
    );
    assert_eq!(
        database.lineage("run-1").expect("lists"),
        ["run-2", "run-1"]
    );
    // Its process is this test, which still runs.
    assert_eq!(run.state, "running");

    let unknown = database.record(&start("run-3", Some("run-9")));
    assert!(matches!(unknown, Err(DatabaseError::UnknownRun(_))));
}

#[test]
fn a_database_of_a_newer_version_is_refused() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("runs.sqlite");
    let database = Database::open(&path).expect("creates");
    assert_eq!(database.version().expect("reads the version"), VERSION);
    drop(database);
    let connection = rusqlite::Connection::open(&path).expect("opens");
    connection
        .pragma_update(None, "user_version", VERSION + 1)
        .expect("sets a newer version");
    drop(connection);
    let error = Database::open(&path).err().expect("refused");
    assert!(error.to_string().contains("newer iunctus"), "{error}");
}
