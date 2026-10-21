//! Checks live task export, shutdown draining, deduplication and collector failures.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use run_database::{Database, Recorder, RunStart};
use run_telemetry::Live;
use serde_json::Value;
use workflow_graph::{Language, Outcome, Task, TaskId};

fn recorder(path: &Path) -> Recorder {
    let start = RunStart {
        id: "live-run".into(),
        resumes: None,
        workflow: "/work/main.nf".into(),
        language: Language::Nextflow,
        revision: None,
        arguments: Vec::new(),
        parameters: Vec::new(),
        executor: "local".into(),
        simulated: false,
        store: None,
        program: None,
        launch: None,
        workflow_folder: None,
        repeats: None,
    };
    Database::open(path).unwrap().record(&start).unwrap()
}

fn task(recorder: &mut Recorder, id: u64, complete: bool) {
    let task = Task {
        id: TaskId(id),
        name: format!("task {id}"),
        step: format!("step {id}"),
        ..Task::default()
    };
    recorder.requested(&task, &[]);
    recorder.started(task.id, 1);
    if complete {
        recorder.finished(
            &Outcome {
                id: task.id,
                ..Outcome::default()
            },
            1,
            false,
        );
        recorder.validated(task.id, 1, true);
    }
}

fn collector(
    count: usize,
    refuses: bool,
) -> (String, Receiver<Value>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (sender, receiver) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        for index in 0..count {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            assert!(first.starts_with("POST /v1/traces "));
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            sender.send(serde_json::from_slice(&body).unwrap()).unwrap();
            let status = if refuses && index == 0 {
                "500 Failed"
            } else {
                "200 OK"
            };
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            )
            .unwrap();
        }
    });
    (endpoint, receiver, thread)
}

fn spans(request: &Value) -> &[Value] {
    request["resourceSpans"][0]["scopeSpans"][0]["spans"]
        .as_array()
        .unwrap()
}

#[test]
fn exports_before_end() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runs.sqlite");
    let mut recorder = recorder(&path);
    let (endpoint, received, thread) = collector(3, false);
    let live = Live::start(
        &path,
        &mut recorder,
        reqwest::Client::new(),
        endpoint,
        Vec::new(),
    )
    .unwrap();
    task(&mut recorder, 1, true);
    let early = received
        .recv_timeout(Duration::from_secs(6))
        .expect("task spans while running");
    assert_eq!(spans(&early).len(), 2);
    assert!(
        spans(&early)
            .iter()
            .all(|span| span.get("parentSpanId").is_some())
    );
    assert!(
        Database::open(&path)
            .unwrap()
            .run("live-run")
            .unwrap()
            .unwrap()
            .ended
            .is_none()
    );
    task(&mut recorder, 2, true);
    recorder.ended(None);
    recorder.close().unwrap();
    live.finish();
    let later = received.recv_timeout(Duration::from_secs(1)).unwrap();
    let final_span = received.recv_timeout(Duration::from_secs(1)).unwrap();
    thread.join().unwrap();
    assert_eq!(spans(&later).len(), 2);
    assert_eq!(spans(&final_span).len(), 1);
    assert_eq!(spans(&final_span)[0]["name"], "run main.nf");
    let mut ids = HashSet::new();
    for request in [&early, &later, &final_span] {
        for span in spans(request) {
            assert!(ids.insert(span["spanId"].as_str().unwrap()));
        }
    }
    assert_eq!(ids.len(), 5);
}

#[test]
fn drains_interrupted_tasks() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runs.sqlite");
    let mut recorder = recorder(&path);
    let (endpoint, received, thread) = collector(2, false);
    let live = Live::start(
        &path,
        &mut recorder,
        reqwest::Client::new(),
        endpoint,
        Vec::new(),
    )
    .unwrap();
    task(&mut recorder, 1, false);
    recorder.ended(Some("interrupted"));
    recorder.close().unwrap();
    live.finish();
    let pending = received.recv_timeout(Duration::from_secs(1)).unwrap();
    let root = received.recv_timeout(Duration::from_secs(1)).unwrap();
    thread.join().unwrap();
    assert_eq!(spans(&pending).len(), 2);
    assert_eq!(spans(&pending)[0]["status"]["code"], 2);
    assert_eq!(
        spans(&pending)[1]["endTimeUnixNano"],
        spans(&root)[0]["endTimeUnixNano"]
    );
    assert_eq!(spans(&root)[0]["status"]["message"], "interrupted");
}

struct Warnings(Arc<AtomicUsize>);

impl tracing::Subscriber for Warnings {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        if *event.metadata().level() == tracing::Level::WARN {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[test]
fn endpoint_failure_warns() {
    let warnings = Arc::new(AtomicUsize::new(0));
    let _subscriber = tracing::subscriber::set_default(Warnings(warnings.clone()));
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runs.sqlite");
    let mut recorder = recorder(&path);
    let (endpoint, received, thread) = collector(2, true);
    let live = Live::start(
        &path,
        &mut recorder,
        reqwest::Client::new(),
        endpoint,
        Vec::new(),
    )
    .unwrap();
    task(&mut recorder, 1, true);
    recorder.ended(None);
    recorder.close().unwrap();
    live.finish();
    let tasks = received.recv_timeout(Duration::from_secs(1)).unwrap();
    let root = received.recv_timeout(Duration::from_secs(1)).unwrap();
    thread.join().unwrap();
    assert_eq!(spans(&tasks).len(), 2);
    assert_eq!(spans(&root).len(), 1);
    assert!(warnings.load(Ordering::SeqCst) > 0);
    assert_eq!(
        Database::open(&path)
            .unwrap()
            .run("live-run")
            .unwrap()
            .unwrap()
            .state,
        "succeeded"
    );
}
