//! Records runs into run stores of a host and reads them back like the run database file.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::Arc;

use run_database::{
    Database, DatabaseError, MemoryStore, Recorder, RunReader, RunStart, RunStore, SqliteStore,
};
use workflow_graph::{Edge, EdgeKind, Language, Outcome, Task, TaskId};

fn start(id: &str, resumes: Option<&str>) -> RunStart {
    RunStart {
        id: id.into(),
        resumes: resumes.map(str::to_owned),
        workflow: "/work/main.nf".into(),
        language: Language::Nextflow,
        revision: None,
        arguments: vec!["run".into()],
        parameters: Vec::new(),
        executor: "local".into(),
        simulated: false,
        store: None,
        program: None,
        launch: None,
        workflow_folder: None,
        repeats: None,
    }
}

fn task(id: u64) -> Task {
    Task {
        id: TaskId(id),
        name: format!("MAKE ({id})"),
        step: "MAKE".into(),
        command: vec!["true".into()],
        ..Task::default()
    }
}

/// Records two tasks, the second after the first, and ends the run.
fn record(mut recorder: Recorder) -> Result<(), DatabaseError> {
    let edge = Edge {
        from: TaskId(1),
        to: TaskId(2),
        kind: EdgeKind::Order,
    };
    recorder.requested(&task(1), &[]);
    recorder.started(TaskId(1), 1);
    let done = Outcome {
        id: TaskId(1),
        stdout: "made\n".into(),
        ..Outcome::default()
    };
    recorder.finished(&done, 1, false);
    recorder.requested(&task(2), &[edge]);
    recorder.started(TaskId(2), 1);
    let failed = Outcome {
        id: TaskId(2),
        exit_code: 3,
        ..Outcome::default()
    };
    recorder.finished(&failed, 1, false);
    recorder.delivered("results/out.txt", "/store/out.txt");
    recorder.ended(Some("task 2 failed"));
    recorder.close()
}

/// What a reader says of the recorded run, as plain values to compare.
fn summary(reader: &dyn RunReader, run: &str) -> (String, Vec<String>, usize, usize, i64) {
    let found = reader.run(run).unwrap().expect("the run");
    let states = (reader.tasks(run).unwrap().into_iter())
        .map(|row| format!("{}={}", row.id, row.state))
        .collect();
    let edges = reader.edges(run).unwrap().len();
    let logs = usize::from(reader.log(run, 1, 1, "stdout").unwrap().is_some());
    (
        found.state,
        states,
        edges,
        logs,
        reader.last_event(run).unwrap(),
    )
}

#[test]
fn memory_matches_sqlite() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("runs.sqlite");
    record(
        Database::open(&path)
            .unwrap()
            .record(&start("run-1", None))
            .unwrap(),
    )
    .unwrap();
    let memory: Arc<dyn RunStore> = Arc::new(MemoryStore::new().unwrap());
    record(Recorder::with_store(memory.clone(), &start("run-1", None)).unwrap()).unwrap();
    let database = Database::open(&path).unwrap();
    let expected = summary(&database, "run-1");
    assert_eq!(expected.0, "failed");
    assert_eq!(expected.1, ["1=succeeded", "2=failed"]);
    assert_eq!(summary(memory.as_ref(), "run-1"), expected);
    let delivered = memory.deliveries("run-1").unwrap();
    assert_eq!(delivered[0].location, "/store/out.txt");
}

#[test]
fn shares_sqlite_file() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("runs.sqlite");
    let store: Arc<dyn RunStore> = Arc::new(SqliteStore::open(&path).unwrap());
    record(Recorder::with_store(store.clone(), &start("first", None)).unwrap()).unwrap();
    let resumed = Recorder::with_store(store.clone(), &start("second", Some("first"))).unwrap();
    record(resumed).unwrap();
    let database = Database::open(&path).unwrap();
    assert_eq!(database.lineage("first").unwrap(), ["second", "first"]);
    assert_eq!(store.lineage("first").unwrap(), ["second", "first"]);
    let error = Recorder::with_store(store, &start("third", Some("missing")));
    assert!(matches!(error, Err(DatabaseError::UnknownRun(_))));
}

#[test]
fn store_notifies_commits() {
    let store = Arc::new(MemoryStore::new().unwrap());
    let mut recorder = Recorder::with_store(store.clone(), &start("run-1", None)).unwrap();
    let commits = recorder.commits();
    recorder.requested(&task(1), &[]);
    commits.try_recv().expect("a change was applied");
    assert_eq!(store.tasks("run-1").unwrap()[0].state, "waiting");
    let change = run_database::Change::Started {
        task: TaskId(1),
        attempt: 1,
    };
    assert!(store.apply("missing", change).is_err(), "an unknown run");
    recorder.close().unwrap();
    assert!(commits.recv().is_err(), "closing ends the notifications");
}

#[test]
fn concurrent_writers_wait() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("runs.sqlite");
    let ready = Arc::new(std::sync::Barrier::new(2));
    let writers: Vec<_> = ["left", "right"]
        .into_iter()
        .map(|run| {
            // Each writer has a connection of its own, as two processes would.
            let store = SqliteStore::open(&path).unwrap();
            store.begin(&start(run, None)).unwrap();
            let ready = ready.clone();
            std::thread::spawn(move || {
                ready.wait();
                for id in 1..=40 {
                    let requested = run_database::Change::Requested {
                        task: &task(id),
                        edges: &[],
                    };
                    store.apply(run, requested)?;
                }
                Ok::<_, DatabaseError>(())
            })
        })
        .collect();
    for writer in writers {
        writer
            .join()
            .unwrap()
            .expect("no write fails on the busy file");
    }
    let database = Database::open(&path).unwrap();
    for run in ["left", "right"] {
        assert_eq!(database.tasks(run).unwrap().len(), 40);
        assert_eq!(database.last_event(run).unwrap(), 40);
    }
}
