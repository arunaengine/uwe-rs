//! A recorded run as OpenTelemetry traces: the spans of the run, its tasks and attempts, their
//! attributes and events, and sending them to a small OTLP HTTP receiver of the test.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};

use run_database::{Database, RunStart};
use run_telemetry::{Request, Span, TelemetryError, Value, requests, send, write_lines};
use workflow_graph::{Language, Outcome, Task, TaskId, Usage};

/// Records a real run: ALIGN ran and was marked, MERGE failed.
fn record(path: &Path) {
    let start = RunStart {
        id: "run-1".into(),
        resumes: None,
        workflow: "/work/main.nf".into(),
        language: Language::Nextflow,
        revision: Some("0123abc".into()),
        arguments: vec!["run".into(), "main.nf".into()],
        parameters: vec![("reads".into(), "*.fq".into())],
        executor: "local".into(),
        simulated: false,
        store: Some("/store".into()),
        program: None,
        launch: None,
        workflow_folder: None,
        repeats: None,
    };
    let mut recorder = Database::open(path)
        .and_then(|database| database.record(&start))
        .expect("starts a record");
    let task = |id: u64, step: &str| Task {
        id: TaskId(id),
        name: format!("{step} (s1)"),
        step: step.into(),
        image: Some("debian:stable-slim".into()),
        command: vec!["true".into()],
        ..Task::default()
    };
    let align = task(1, "ALIGN");
    recorder.requested(&align, &[]);
    recorder.started(align.id, 1);
    recorder.finished(
        &Outcome {
            id: align.id,
            image: Some("debian@sha256:5bc3".into()),
            usage: Some(Usage {
                cpu_usec: Some(1_500_000),
                memory_peak: Some(4096),
                ..Usage::default()
            }),
            ..Outcome::default()
        },
        1,
        false,
    );
    recorder.marked(
        align.id,
        "content",
        "the workflow read out.txt while it planned",
    );
    let merge = task(2, "MERGE");
    recorder.requested(&merge, &[]);
    recorder.started(merge.id, 1);
    let failed = Outcome {
        id: merge.id,
        exit_code: 1,
        ..Outcome::default()
    };
    recorder.finished(&failed, 1, false);
    recorder.ended(Some("task MERGE (s1) failed"));
    recorder.close().expect("records the run");
}

fn spans(requests: &[Request]) -> Vec<&Span> {
    requests.iter().flat_map(Request::spans).collect()
}

fn text<'a>(span: &'a Span, key: &str) -> Option<&'a Value> {
    (span.attributes.iter())
        .find(|attribute| attribute.key == key)
        .map(|attribute| &attribute.value)
}

#[test]
fn traces_task_attempts() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("runs.sqlite");
    record(&path);
    let database = Database::open(&path).expect("opens again");
    let requests = requests(&database, "run-1", 2)
        .expect("reads the run")
        .expect("knows the run");
    assert_eq!(requests.len(), 3, "5 spans, 2 per request");
    let spans = spans(&requests);
    let names: Vec<&str> = spans.iter().map(|span| span.name.as_str()).collect();
    assert_eq!(
        names,
        ["run main.nf", "ALIGN", "attempt 1", "MERGE", "attempt 1"]
    );
    let [run, align, attempt, merge, _] = spans.as_slice() else {
        panic!("five spans");
    };
    assert!(spans.iter().all(|span| span.trace_id == run.trace_id));
    assert!(run.parent_span_id.is_empty());
    assert_eq!(align.parent_span_id, run.span_id);
    assert_eq!(attempt.parent_span_id, align.span_id);
    assert_eq!(
        (run.status.code, run.status.message.as_str()),
        (2, "task MERGE (s1) failed")
    );
    assert_eq!(
        (merge.status.code, merge.status.message.as_str()),
        (2, "exit code 1")
    );
    assert_eq!(align.status.code, 1);
    assert_eq!(
        text(run, "uwe.parameter.reads"),
        Some(&Value::Text("*.fq".into()))
    );
    assert_eq!(
        text(attempt, "uwe.attempt.cpu_usec"),
        Some(&Value::Integer("1500000".into()))
    );
    assert_eq!(
        text(attempt, "uwe.attempt.image"),
        Some(&Value::Text("debian@sha256:5bc3".into()))
    );
    let marker = (align.events.iter())
        .find(|event| event.name == "marked")
        .expect("the marker as an event");
    let kinds: Vec<&str> = marker
        .attributes
        .iter()
        .map(|value| value.key.as_str())
        .collect();
    assert_eq!(kinds, ["uwe.marker.kind", "uwe.marker.reason"]);
    let events: Vec<&str> = attempt
        .events
        .iter()
        .map(|event| event.name.as_str())
        .collect();
    assert_eq!(events, ["started", "finished"]);

    let json = serde_json::to_value(&requests[0]).expect("JSON");
    let first = &json["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
    assert_eq!(
        json["resourceSpans"][0]["resource"]["attributes"][0]["key"],
        "service.name"
    );
    assert!(first["startTimeUnixNano"].is_string(), "{first}");
    assert_eq!(first["kind"], 1);
    let mut lines = Vec::new();
    write_lines(&requests, &mut lines).expect("writes");
    assert_eq!(String::from_utf8_lossy(&lines).lines().count(), 3);
    assert_eq!(requests_of(&database, "no-run"), None);
}

fn requests_of(database: &Database, run: &str) -> Option<Vec<Request>> {
    requests(database, run, 10).expect("reads")
}

#[test]
fn redacts_trace_passwords() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("runs.sqlite");
    let private = "https://user:password@host/path";
    let start = RunStart {
        id: "private-run".into(),
        resumes: None,
        workflow: "/work/private.nf".into(),
        language: Language::Nextflow,
        revision: None,
        arguments: Vec::new(),
        parameters: vec![(private.into(), private.into())],
        executor: private.into(),
        simulated: false,
        store: None,
        program: None,
        launch: None,
        workflow_folder: None,
        repeats: None,
    };
    let mut recorder = Database::open(&path)
        .and_then(|database| database.record(&start))
        .expect("starts a private record");
    let task = Task {
        id: TaskId(1),
        name: private.into(),
        step: private.into(),
        image: Some(private.into()),
        command: vec!["false".into()],
        ..Task::default()
    };
    recorder.requested(&task, &[]);
    recorder.ended(Some(private));
    recorder.close().expect("records private text");

    let database = Database::open(&path).expect("opens again");
    let requests = requests_of(&database, "private-run").expect("knows the run");
    let spans = spans(&requests);
    let [run, task] = spans.as_slice() else {
        panic!("run and task spans");
    };
    let public = "https://user:[secret]@host/path";
    assert_eq!(
        text(run, &format!("uwe.parameter.{public}")),
        Some(&Value::Text(public.into()))
    );
    assert_eq!(text(run, "uwe.executor"), Some(&Value::Text(public.into())));
    assert_eq!(task.name, public);
    assert_eq!(
        text(task, "uwe.task.name"),
        Some(&Value::Text(public.into()))
    );
    assert_eq!(run.status.message, public);

    let mut lines = Vec::new();
    write_lines(&requests, &mut lines).expect("writes traces");
    let written = String::from_utf8(lines).expect("UTF-8 JSON lines");
    assert!(written.contains(public), "{written}");
    assert!(!written.contains("password"), "{written}");

    let stored = database
        .run("private-run")
        .expect("reads private run")
        .expect("stored run");
    assert_eq!(
        stored.parameters,
        vec![(private.to_owned(), private.to_owned())]
    );
    assert_eq!(stored.executor, private);
    assert_eq!(stored.error.as_deref(), Some(private));
    let stored_tasks = database.tasks("private-run").expect("reads private tasks");
    assert_eq!(stored_tasks[0].name, private);
    assert_eq!(stored_tasks[0].step, private);
}

#[test]
fn sends_configured_headers() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("runs.sqlite");
    record(&path);
    let database = Database::open(&path).expect("opens again");
    let requests = requests(&database, "run-1", 2)
        .expect("reads")
        .expect("the run");
    let (endpoint, received) = receiver("200 OK", r#"{"partialSuccess":{"rejectedSpans":"1"}}"#);
    let client = reqwest::Client::new();
    let headers = [("authorization".to_owned(), "Bearer secret-token".to_owned())];
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let sent = runtime
        .block_on(send(&client, &endpoint, &headers, &requests))
        .expect("sends");
    assert_eq!((sent.requests, sent.spans), (3, 5));
    assert_eq!(sent.rejected, 3, "each answer rejects one span");
    let (head, body) = received.recv().expect("a request");
    assert!(head.starts_with("POST /v1/traces HTTP/1.1"), "{head}");
    assert!(
        head.to_ascii_lowercase()
            .contains("authorization: bearer secret-token"),
        "{head}"
    );
    assert!(
        head.to_ascii_lowercase()
            .contains("content-type: application/json"),
        "{head}"
    );
    let request: serde_json::Value = serde_json::from_str(&body).expect("JSON");
    assert_eq!(
        request["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["name"],
        "run main.nf"
    );

    let (refusing, _) = receiver("400 Bad Request", "not OTLP");
    let error = runtime
        .block_on(send(&client, &refusing, &[], &requests))
        .expect_err("refused");
    assert!(
        matches!(error, TelemetryError::Refused { status: 400, .. }),
        "{error}"
    );
}

/// A small OTLP HTTP receiver: it answers every request with this status and answer, and
/// hands the head and body of each request to the channel.
fn receiver(status: &'static str, answer: &'static str) -> (String, Receiver<(String, String)>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listens");
    let address = format!("http://{}", listener.local_addr().expect("an address"));
    let (sender, received) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let sender = sender.clone();
            std::thread::spawn(move || serve(stream, status, answer, &sender));
        }
    });
    (address, received)
}

/// Answers the requests of one connection, which keeps going until the client closes it.
fn serve(stream: TcpStream, status: &str, answer: &str, sender: &Sender<(String, String)>) {
    let mut reader = BufReader::new(stream.try_clone().expect("a second handle"));
    let mut writer = stream;
    loop {
        let mut head = String::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            if line == "\r\n" {
                break;
            }
            head.push_str(&line);
        }
        let length = head
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.trim().parse().ok())
            .unwrap_or(0);
        let mut body = vec![0; length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let _ = sender.send((head, String::from_utf8_lossy(&body).into_owned()));
        let reply = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{answer}",
            answer.len()
        );
        if writer.write_all(reply.as_bytes()).is_err() {
            return;
        }
    }
}
