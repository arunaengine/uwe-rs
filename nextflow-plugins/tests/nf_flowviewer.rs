//! nf-flowviewer against the plugin 1.0.0 with Nextflow 26.04.6: a real run of the probe sends a
//! local FlowViewer server the recorded requests, with the bodies of the recorded shape.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use nextflow_evaluator::interpreter::Host;
use nextflow_runtime::driver::{Nextflow, Settings};
use serde_json::Value as Json;
use workflow_graph::{
    Attempt, Engine, Executor, Files, Launch, LocalFiles, Outcome, OutputFile, Running,
};

struct Quiet;

impl Host for Quiet {
    fn print(&mut self, _: &str) {}
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// A request as the server got it: method, path and JSON body.
type Request = (String, String, Json);

/// Serves on a free local port, answers the session with 201 and id 7, everything else with
/// 200, and keeps the requests.
fn serve() -> (String, Arc<Mutex<Vec<Request>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let base = format!("http://{}", listener.local_addr().expect("an address"));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let kept = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().expect("a stream"));
            let mut first = String::new();
            reader.read_line(&mut first).expect("reads");
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("reads");
                if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().expect("a length");
                }
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).expect("reads the body");
            let mut words = first.split_whitespace();
            let method = words.next().unwrap_or_default().to_owned();
            let path = words.next().unwrap_or_default().to_owned();
            let session = method == "POST" && path == "/api/session";
            let (status, answer) = match session {
                true => ("201 Created", "{\"id\": 7}"),
                false => ("200 OK", "{}"),
            };
            let body = serde_json::from_slice(&body).unwrap_or(Json::Null);
            kept.lock()
                .expect("not poisoned")
                .push((method, path, body));
            let reply = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n{answer}",
                answer.len()
            );
            let _ = (&stream).write_all(reply.as_bytes());
        }
    });
    (base, requests)
}

/// Writes what the scripts of the probe write: SPLIT a part per line, SHOUT its part in
/// capitals after the greeting.
struct Probe(PathBuf);

impl Executor for Probe {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        let folder = self.0.join(task.id.to_string());
        std::fs::create_dir_all(&folder).expect("creates");
        let input = (task.inputs.iter())
            .find(|input| input.content.is_none())
            .expect("an input file");
        let text = std::fs::read_to_string(&input.source).expect("reads");
        let made: Vec<(String, String)> = match task.step.as_str() {
            "SPLIT" => (text.lines().enumerate())
                .map(|(place, line)| (format!("part_{place:02}.txt"), format!("{line}\n")))
                .collect(),
            _ => {
                let base = input.path.trim_end_matches(".txt");
                vec![(
                    format!("{base}.up.txt"),
                    format!("hello\n{}", text.to_uppercase()),
                )]
            }
        };
        let files = (made.into_iter())
            .map(|(name, text)| {
                std::fs::write(folder.join(&name), text).expect("writes");
                OutputFile {
                    location: folder.join(&name).to_string_lossy().into_owned(),
                    path: name,
                    ..OutputFile::default()
                }
            })
            .collect();
        Running::ended(Ok(Outcome {
            id: task.id,
            files,
            ..Outcome::default()
        }))
    }

    fn files(&self) -> Option<Arc<dyn Files>> {
        Some(Arc::new(LocalFiles))
    }
}

/// The keys of a value and of the values inside it, by their path.
fn keys(value: &Json, at: &str) -> Vec<String> {
    match value {
        Json::Object(entries) => (entries.iter())
            .flat_map(|(key, inner)| {
                let path = format!("{at}/{key}");
                std::iter::once(path.clone()).chain(keys(inner, &path))
            })
            .collect(),
        Json::Array(items) => items.first().map_or_else(Vec::new, |item| keys(item, at)),
        _ => Vec::new(),
    }
}

/// What a request is, by its method, path and status, with the keys of its body.
fn kind(request: &Request) -> (String, Vec<String>) {
    let status = request.2["status"].as_str().unwrap_or_default();
    let name = format!("{} {} {status}", request.0, request.1);
    let mut found = keys(&request.2, "");
    found.sort();
    (name, found)
}

#[test]
fn sends_recorded_requests() {
    let launch = tempfile::tempdir().expect("a folder");
    let folder = launch.path();
    let at = folder.to_string_lossy();
    let (server, requests) = serve();
    let probe = fixtures().join("nf_fairscape");
    let script = std::fs::read_to_string(probe.join("main.nf")).expect("reads");
    let script = script.replace("publishDir '", &format!("publishDir '{at}/"));
    std::fs::write(folder.join("main.nf"), script).expect("writes");
    std::fs::copy(probe.join("input.txt"), folder.join("input.txt")).expect("copies");
    let config = format!(
        "plugins {{ id 'nf-flowviewer@1.0.0' }}\nflowViewer {{\n    serverUrl = '{server}'\n    \
         workflowName = 'probe-project'\n    runBy = 'tester'\n    artifactMaxSize = 16\n}}\n"
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes");
    let run = Launch {
        target: folder.to_owned(),
        scratch: folder.join("scratch"),
        ..Launch::default()
    };
    let settings = Settings {
        host: Box::new(Quiet),
        ..Settings::default()
    };
    let mut driver = Nextflow::new(&run, settings).expect("a driver");
    let execution = Engine::new(&mut Probe(folder.join("tasks"))).run("main.nf", &mut driver);
    assert!(execution.error.is_none(), "{:?}", execution.error);
    drop(driver);
    let recorded = std::fs::read_to_string(fixtures().join("nf_flowviewer/requests.jsonl"));
    let recorded: Vec<Request> = (recorded.expect("reads").lines())
        .map(|line| {
            let line: Json = serde_json::from_str(line).expect("JSON");
            let body = serde_json::from_str(line["body"].as_str().unwrap_or("null"));
            let method = line["method"].as_str().unwrap_or_default().to_owned();
            let path = line["path"].as_str().unwrap_or_default().to_owned();
            (method, path, body.expect("a JSON body"))
        })
        .collect();
    let sent = requests.lock().expect("not poisoned").clone();
    let mut sent_kinds: Vec<_> = sent.iter().map(kind).collect();
    let mut recorded_kinds: Vec<_> = recorded.iter().map(kind).collect();
    // uwe's graph has no operator vertices, and so no edge labels or operator names.
    assert_eq!(sent_kinds[..3], recorded_kinds[..3]);
    assert_eq!(sent_kinds.last(), recorded_kinds.last());
    sent_kinds.sort();
    recorded_kinds.sort();
    assert_eq!(sent_kinds, recorded_kinds);
    let session = &sent[0].2["sessionData"];
    assert_eq!(session["workflowName"], "probe-project");
    assert_eq!(session["runBy"], "tester");
    let labels: Vec<&Json> = (sent[1].2["processes"].as_array().into_iter().flatten())
        .map(|process| &process["label"])
        .collect();
    assert_eq!(labels, ["SPLIT", "SHOUT"]);
    let artifacts = sent.iter().filter(|request| request.1 == "/api/artifacts");
    let data: Vec<&str> = artifacts
        .map(|request| request.2["artifact"]["data"].as_str().unwrap_or_default())
        .collect();
    assert!(
        data.contains(&"c\n") && data.contains(&"hello\nB\n"),
        "{data:?}"
    );
}
