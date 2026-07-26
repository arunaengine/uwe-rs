//! The insight of the run database: where the time of a run went, and tasks that the workflow
//! asks for again to retry them.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::time::{Duration, SystemTime};

use run_database::{Database, Recorder, RunStart};
use workflow_graph::{Edge, EdgeKind, Language, Outcome, OutputFile, Task, TaskId, Usage};

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

/// A time some milliseconds after `base`.
fn at(base: SystemTime, milliseconds: u64) -> Option<SystemTime> {
    Some(base + Duration::from_millis(milliseconds))
}

/// An attempt that ran from `from` to `to` milliseconds after `base`, and used this much CPU.
fn ran(id: u64, base: SystemTime, from: u64, to: u64, cpu_usec: u64) -> Outcome {
    Outcome {
        id: TaskId(id),
        files: vec![OutputFile {
            path: "out.txt".into(),
            location: format!("/store/runs/r/{id}/out.txt"),
            digest: Some(format!("sha256:{id:064}")),
            size: Some(id),
            ..OutputFile::default()
        }],
        started: at(base, from),
        ended: at(base, to),
        usage: Some(Usage {
            cpu_usec: Some(cpu_usec),
            memory_peak: Some(cpu_usec * 10),
            ..Usage::default()
        }),
        ..Outcome::default()
    }
}

fn file_edge(from: u64, to: u64) -> Edge {
    Edge {
        from: TaskId(from),
        to: TaskId(to),
        kind: EdgeKind::File {
            path: "in.txt".into(),
        },
    }
}

/// Records one attempt of a task that the driver asked for with these edges.
fn attempt(recorder: &mut Recorder, task: &Task, edges: &[Edge], outcome: &Outcome) {
    recorder.requested(task, edges);
    recorder.started(task.id, 1);
    recorder.finished(outcome, 1, false);
}

#[test]
fn the_time_of_a_run_follows_the_chain_that_ended_last() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("runs.sqlite");
    let mut recorder = Database::open(&path)
        .and_then(|database| database.record(&start("run-1")))
        .expect("starts a record");
    // A and B ran at once; C reads both and ran after B, the one that ended last.
    let base = SystemTime::now() - Duration::from_secs(10);
    let (a, b) = (task(1, "ALIGN"), task(2, "ALIGN"));
    recorder.requested(&a, &[]);
    recorder.requested(&b, &[]);
    recorder.started(a.id, 1);
    recorder.started(b.id, 1);
    recorder.finished(&ran(1, base, 0, 100, 2_000), 1, false);
    recorder.finished(&ran(2, base, 0, 300, 5_000), 1, false);
    let edges = [file_edge(1, 3), file_edge(2, 3)];
    attempt(
        &mut recorder,
        &task(3, "MERGE"),
        &edges,
        &ran(3, base, 350, 500, 1_000),
    );
    recorder.ended(None);
    recorder.close().expect("records the run");

    let database = Database::open(&path).expect("opens again");
    let spent = database
        .time_spent("run-1")
        .expect("reads the run")
        .expect("knows the run");
    let chain: Vec<u64> = spent.critical_path.iter().map(|task| task.task).collect();
    assert_eq!(chain, [2, 3]);
    assert_eq!(spent.critical_path[0].ran, 300);
    assert_eq!(spent.critical_path[1].ran, 150);
    assert_eq!((spent.tasks, spent.reused), (3, 0));
    assert_eq!(spent.ran, 100 + 300 + 150);
    let align = &spent.steps[0];
    assert_eq!((align.step.as_str(), align.tasks), ("ALIGN", 2));
    assert_eq!(align.cpu_usec, Some(7_000));
    assert_eq!(align.memory_peak, Some(50_000));
    assert_eq!(database.time_spent("no-run").expect("reads"), None);
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
