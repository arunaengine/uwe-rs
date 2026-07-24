//! The insight of the run database: tasks that the workflow asks for again to retry them.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use run_database::{Database, RunStart};
use workflow_graph::{Language, Outcome, Task, TaskId};

fn start(id: &str) -> RunStart {
    RunStart {
        id: id.into(),
        resumes: None,
        workflow: "/work/main.nf".into(),
        language: Language::Nextflow,
        revision: None,
        arguments: vec!["run".into(), "main.nf".into()],
        parameters: Vec::new(),
        executor: "local".into(),
        simulated: false,
        store: Some("/store".into()),
    }
}

fn task(id: u64, step: &str) -> Task {
    Task {
        id: TaskId(id),
        name: format!("{step} ({id})"),
        step: step.into(),
        image: Some("debian:stable-slim".into()),
        command: vec!["bash".into(), "-ue".into(), ".command.sh".into()],
        outputs: vec!["out.txt".into()],
        ..Task::default()
    }
}

#[test]
fn a_task_asked_for_again_keeps_its_record() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("runs.sqlite");
    let mut recorder = Database::open(&path)
        .and_then(|database| database.record(&start("run-2")))
        .expect("starts a record");
    let first = task(1, "SORT");
    recorder.requested(&first, &[]);
    recorder.started(first.id, 1);
    let failed = Outcome {
        id: first.id,
        exit_code: 137,
        ..Outcome::default()
    };
    recorder.finished(&failed, 1, false);
    // The workflow retries it with more memory.
    let mut again = first.clone();
    again.resources.memory_bytes = Some(2 << 30);
    recorder.requested(&again, &[]);
    recorder.started(again.id, 2);
    recorder.finished(
        &Outcome {
            id: again.id,
            ..Outcome::default()
        },
        2,
        false,
    );
    recorder.ended(None);
    recorder.close().expect("records the whole run");

    let database = Database::open(&path).expect("opens again");
    let detail = database.task("run-2", 1).expect("reads").expect("the task");
    assert_eq!(detail.row.state, "succeeded");
    assert_eq!(detail.attempts.len(), 2);
    assert_eq!(detail.description.resources.memory_bytes, Some(2 << 30));
    let events = database.events("run-2", 0, 100).expect("reads the events");
    let requested = events.iter().filter(|event| event.kind == "requested");
    assert_eq!(requested.count(), 2);
    assert!(Path::new(&path).exists());
}
