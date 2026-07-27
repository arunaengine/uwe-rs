//! The insight views of the page: where the time of a run went.
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
    let mut recorder = Database::open(&path)
        .and_then(|database| database.record(&start("run-1")))
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
    path
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
