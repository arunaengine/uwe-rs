//! The insight views of the page: where the time of a run went, and comparing two runs.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use run_database::{Database, RunStart};
use run_page::{Server, Settings};
use serde_json::Value;
use workflow_graph::{Edge, EdgeKind, Language, Outcome, Task, TaskId};

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
        store: None,
        program: None,
        launch: None,
        workflow_folder: None,
        repeats: None,
    }
}

fn task(id: u64, step: &str) -> Task {
    Task {
        id: TaskId(id),
        name: format!("{step} ({id})"),
        step: step.into(),
        image: Some("debian:stable-slim".into()),
        command: vec!["true".into()],
        ..Task::default()
    }
}

/// Records a run of two tasks, the second after the first.
fn record(directory: &Path) -> PathBuf {
    let path = directory.join("runs.sqlite");
    record_run(&path, "run-1");
    path
}

/// Records a run of two tasks in the database at `path`.
fn record_run(path: &Path, id: &str) {
    let mut recorder = Database::open(path)
        .and_then(|database| database.record(&start(id)))
        .expect("starts a record");
    let order = Edge {
        from: TaskId(1),
        to: TaskId(2),
        kind: EdgeKind::Order,
    };
    for (task, edges) in [(task(1, "FIRST"), vec![]), (task(2, "SECOND"), vec![order])] {
        recorder.requested(&task, &edges);
        recorder.started(task.id, 1);
        recorder.finished(
            &Outcome {
                id: task.id,
                ..Outcome::default()
            },
            1,
            false,
        );
    }
    recorder.ended(None);
    recorder.close().expect("records the run");
}

fn get(url: &str) -> (u16, String) {
    let response = reqwest::blocking::get(url).expect("the page answers");
    let status = response.status().as_u16();
    (status, response.text().expect("a text"))
}

#[test]
fn the_page_tells_where_the_time_went() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let server = Server::start(&Settings::new(record(directory.path()))).expect("starts");
    let url = server.url().to_owned();

    let (status, script) = get(&format!("{url}/time.js"));
    assert_eq!(status, 200);
    assert!(script.contains("function markPath"), "{script}");
    let (status, text) = get(&format!("{url}/api/runs/run-1/time"));
    assert_eq!(status, 200, "{text}");
    let time: Value = serde_json::from_str(&text).expect("JSON");
    let path: Vec<&Value> = (time["critical_path"].as_array().into_iter().flatten())
        .map(|task| &task["task"])
        .collect();
    assert_eq!(path, [1, 2]);
    assert_eq!(time["steps"][0]["step"], "FIRST");
    let (status, _) = get(&format!("{url}/api/runs/no-run/time"));
    assert_eq!(status, 404);
    // The lines of the graph name the boxes they join, so that the page can mark the path.
    let (status, svg) = get(&format!("{url}/api/runs/run-1/graph.svg"));
    assert_eq!(status, 200);
    assert!(svg.contains("data-from=\"t1\" data-to=\"t2\""), "{svg}");
    server.stop();
}

#[test]
fn the_page_compares_two_runs() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = record(directory.path());
    record_run(&path, "run-2");
    let server = Server::start(&Settings::new(path)).expect("starts");
    let url = server.url().to_owned();

    let (status, page) = get(&format!("{url}/compare/run-1/run-2"));
    assert_eq!(status, 200);
    assert!(page.contains("/compare.js"), "{page}");
    let (status, script) = get(&format!("{url}/compare.js"));
    assert_eq!(status, 200);
    assert!(script.contains("function showCompare"), "{script}");
    let (status, text) = get(&format!("{url}/api/compare/run-1/run-2"));
    assert_eq!(status, 200, "{text}");
    let comparison: Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(comparison["first"]["id"], "run-1");
    let pairings: Vec<&Value> = (comparison["tasks"].as_array().into_iter().flatten())
        .map(|pair| &pair["pairing"])
        .collect();
    assert_eq!(pairings, ["same", "same"]);
    let (status, _) = get(&format!("{url}/api/compare/run-1/no-run"));
    assert_eq!(status, 404);
    server.stop();
}

#[test]
fn the_panel_of_a_task_lists_its_replays() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = record(directory.path());
    let mut recorder = Database::open(&path)
        .and_then(|database| database.record(&start("replay-1")))
        .expect("starts a record");
    recorder.replays("run-1", 2);
    recorder.ended(None);
    recorder.close().expect("records the replay");
    let server = Server::start(&Settings::new(path)).expect("starts");
    let url = server.url().to_owned();

    let (status, text) = get(&format!("{url}/api/runs/run-1/tasks/2"));
    assert_eq!(status, 200, "{text}");
    let task: Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(task["replays"][0]["id"], "replay-1");
    let (status, text) = get(&format!("{url}/api/runs/replay-1"));
    assert_eq!(status, 200, "{text}");
    let run: Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(run["run"]["replays"], "run-1");
    assert_eq!(run["run"]["replays_task"], 2);
    let (status, script) = get(&format!("{url}/run.js"));
    assert_eq!(status, 200);
    assert!(script.contains("iunctus replay ${id} ${task}"), "{script}");
    server.stop();
}
