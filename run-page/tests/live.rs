//! Live streams drain recorded backlogs and close when their server stops.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::time::Duration;

use run_database::{Database, RunStart};
use run_page::{Server, Settings};
use workflow_graph::{Language, Outcome, Task, TaskId};

fn record(folder: &Path, count: u64, finished: bool) -> std::path::PathBuf {
    let path = folder.join("runs.sqlite");
    let start = RunStart {
        id: "stream".into(),
        workflow: "main.nf".into(),
        language: Language::Nextflow,
        executor: "simulator".into(),
        simulated: true,
        resumes: None,
        revision: None,
        arguments: Vec::new(),
        parameters: Vec::new(),
        store: None,
        program: None,
        launch: None,
        workflow_folder: None,
        repeats: None,
    };
    let mut recorder = Database::open(&path).unwrap().record(&start).unwrap();
    for id in 1..=count {
        recorder.requested(
            &Task {
                id: TaskId(id),
                ..Task::default()
            },
            &[],
        );
        if finished {
            recorder.started(TaskId(id), 1);
            recorder.finished(
                &Outcome {
                    id: TaskId(id),
                    ..Outcome::default()
                },
                1,
                false,
            );
        }
    }
    if finished {
        recorder.ended(None);
    }
    recorder.close().unwrap();
    path
}

#[test]
fn completes_entire_backlog() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(&Settings::new(record(root.path(), 4000, true))).unwrap();
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let body = client
        .get(format!("{}/api/runs/stream/events?after=0", server.url()))
        .send()
        .unwrap()
        .text()
        .unwrap();
    let updates: Vec<serde_json::Value> = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(updates.len(), 2);
    assert_eq!(updates[0]["complete"], false);
    assert_eq!(updates[1]["complete"], true);
    let ids: std::collections::BTreeSet<u64> = updates
        .iter()
        .flat_map(|update| update["tasks"].as_array().unwrap())
        .map(|task| {
            assert_eq!(task["state"], "succeeded");
            task["id"].as_u64().unwrap()
        })
        .collect();
    assert_eq!(ids.len(), 4000);
    server.stop();
}

#[test]
fn shutdown_closes_stream() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(&Settings::new(record(root.path(), 1, false))).unwrap();
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
        .get(format!("{}/api/runs/stream/events?after=0", server.url()))
        .send()
        .unwrap();
    let mut reader = BufReader::new(response);
    let mut line = String::new();
    loop {
        line.clear();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line.starts_with("data:") {
            break;
        }
    }
    let (done, ended) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        server.stop();
        done.send(()).unwrap();
    });
    ended
        .recv_timeout(Duration::from_secs(2))
        .expect("an active SSE client must not hold shutdown open");
    worker.join().unwrap();
    let mut tail = String::new();
    reader
        .read_to_string(&mut tail)
        .expect("the cancelled stream closes cleanly");
}
