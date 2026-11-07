//! Reports and stops the TES tasks of one run against a fake server, and leaves the others.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::json;

use super::*;
use crate::Client;

/// What the fake server knows: the state of each task, and each request line with its
/// Authorization header.
#[derive(Default)]
struct Known {
    states: BTreeMap<String, &'static str>,
    requests: Vec<(String, String)>,
}

impl Known {
    fn with(tasks: &[(&str, &'static str)]) -> Arc<Mutex<Known>> {
        let states = (tasks.iter()).map(|(id, state)| ((*id).to_owned(), *state));
        Arc::new(Mutex::new(Known {
            states: states.collect(),
            requests: Vec::new(),
        }))
    }

    /// The ids of the tasks that got a cancel request.
    fn canceled(&self) -> Vec<String> {
        (self.requests.iter())
            .filter(|(line, _)| line.contains(":cancel"))
            .map(|(line, _)| task_id(line))
            .collect()
    }
}

/// A TES server that keeps the state of its tasks and ends a running task when it is canceled.
fn serve(known: Arc<Mutex<Known>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let (line, token) = request(&mut stream);
            let mut known = known.lock().unwrap();
            known.requests.push((line.clone(), token));
            let id = task_id(&line);
            let cancel = line.contains(":cancel");
            if cancel && known.states.get(&id) == Some(&"RUNNING") {
                known.states.insert(id.clone(), "CANCELED");
            }
            let (status, answer) = match known.states.get(&id) {
                Some(_) if cancel => ("200 OK", "{}".to_owned()),
                Some(state) => ("200 OK", json!({"id": id, "state": state}).to_string()),
                None => ("404 Not Found", "{}".to_owned()),
            };
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{answer}",
                answer.len()
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    address
}

/// Reads one request: its first line and its Authorization header.
fn request(stream: &mut std::net::TcpStream) -> (String, String) {
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    let (mut length, mut token) = (0, String::new());
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap_or(0);
            } else if name.eq_ignore_ascii_case("authorization") {
                token = value.trim().to_owned();
            }
        }
    }
    reader.read_exact(&mut vec![0; length]).unwrap();
    (first.trim().to_owned(), token)
}

/// The task that a request line names, like `running` in `GET .../tasks/running?view=FULL`.
fn task_id(line: &str) -> String {
    (line.split("/tasks/").nth(1))
        .and_then(|rest| rest.split([':', '?', ' ']).next())
        .unwrap_or_default()
        .to_owned()
}

fn backend(address: &str, token: Option<&str>, scratch: &Path) -> TesBackend {
    TesBackend {
        client: Client::new(address, token.map(str::to_owned)).unwrap(),
        run: String::new(),
        scratch: scratch.to_owned(),
        configs: Default::default(),
        staging: None,
        settings: Default::default(),
    }
}

/// Writes the journal of an attempt whose uwe process is gone, as an attempt of `run` with the
/// credentials of `backend` writes it for the task `id`.
fn leave(backend: &TesBackend, folder: &str, run: Option<&str>, id: &str) -> PathBuf {
    let root = backend.scratch.join(folder);
    std::fs::create_dir_all(&root).unwrap();
    File::create(root.join("remote.lock")).unwrap();
    let mut journal = json!({"backend": "TES", "server": backend.client.server(),
        "name": folder, "id": id, "owner": u32::MAX, "host": "previous-boot", "started": "",
        "locked": true, "auth": backend.client.identity(), "submission": "only"});
    if let Some(run) = run {
        journal["run"] = json!(run);
    }
    std::fs::write(root.join("remote.json"), journal.to_string()).unwrap();
    root
}

fn found(attempt: &str, id: &str, state: Option<&str>, outcome: Outcome) -> Orphan {
    Orphan {
        attempt: attempt.into(),
        id: Some(id.into()),
        state: state.map(str::to_owned),
        outcome,
    }
}

#[tokio::test]
async fn stops_running_tasks() {
    let folder = tempfile::tempdir().unwrap();
    let known = Known::with(&[("running", "RUNNING"), ("done", "COMPLETE")]);
    let backend = backend(&serve(known.clone()), None, &folder.path().join("scratch"));
    let running = leave(&backend, "first-sort-1-1", Some("first"), "running");
    let done = leave(&backend, "first-sort-2-1", Some("first"), "done");
    let peers = [&backend];
    let report = backend.report_run("first", &peers).await.unwrap();
    assert_eq!(
        report,
        [
            found(
                "first-sort-1-1",
                "running",
                Some("RUNNING"),
                Outcome::Running
            ),
            found("first-sort-2-1", "done", Some("COMPLETE"), Outcome::Ended),
        ]
    );
    assert!(
        known.lock().unwrap().canceled().is_empty(),
        "the report canceled a task"
    );
    assert!(running.join("remote.json").is_file() && done.join("remote.json").is_file());
    let stopped = backend.stop_run("first", &peers).await.unwrap();
    assert_eq!(
        stopped,
        [
            found(
                "first-sort-1-1",
                "running",
                Some("CANCELED"),
                Outcome::Canceled
            ),
            found("first-sort-2-1", "done", Some("COMPLETE"), Outcome::Ended),
        ]
    );
    assert!(!running.join("remote.json").exists() && !done.join("remote.json").exists());
    let asked = {
        let known = known.lock().unwrap();
        assert_eq!(known.states["running"], "CANCELED");
        assert_eq!(known.states["done"], "COMPLETE");
        known.requests.len()
    };
    // A second call finds no journal of the run and asks the server nothing.
    assert!(backend.stop_run("first", &peers).await.unwrap().is_empty());
    assert!(
        backend
            .report_run("first", &peers)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(known.lock().unwrap().requests.len(), asked);
}

#[tokio::test]
async fn keeps_other_attempts() {
    let folder = tempfile::tempdir().unwrap();
    let tasks = ["prefixed", "second", "legacy", "live", "token"].map(|id| (id, "RUNNING"));
    let known = Known::with(&tasks);
    let address = serve(known.clone());
    let scratch = folder.path().join("scratch");
    let backend = backend(&address, None, &scratch);
    // A run whose id starts with `first-`, another run, and a journal of no named run.
    let others = [
        leave(&backend, "first-2-sort-1-1", Some("first-2"), "prefixed"),
        leave(&backend, "second-sort-1-1", Some("second"), "second"),
        leave(&backend, "first-old-1-1", None, "legacy"),
    ];
    // An attempt whose uwe process still holds its lock.
    let live = leave(&backend, "first-live-1-1", Some("first"), "live");
    let lock = File::options()
        .write(true)
        .open(live.join("remote.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    // An attempt with credentials that the call does not have.
    let foreign = leave(
        &self::backend(&address, Some("other-token"), &scratch),
        "first-token-1-1",
        Some("first"),
        "token",
    );
    let roots = [&others[..], &[live.clone(), foreign]].concat();
    let before: Vec<String> = (roots.iter())
        .map(|root| std::fs::read_to_string(root.join("remote.json")).unwrap())
        .collect();
    let peers = [&backend];
    let report = backend.report_run("first", &peers).await.unwrap();
    let foreign = "another TES server or token owns the task";
    let unnamed = "the journal names no run, so it may belong to another";
    let mut expected = [
        found("first-live-1-1", "live", Some("RUNNING"), Outcome::Running),
        found(
            "first-old-1-1",
            "legacy",
            None,
            Outcome::Unclear(unnamed.into()),
        ),
        found(
            "first-token-1-1",
            "token",
            None,
            Outcome::Unclear(foreign.into()),
        ),
    ];
    assert_eq!(report, expected);
    let stopped = backend.stop_run("first", &peers).await.unwrap();
    expected[0].outcome = Outcome::Kept("the uwe process of the attempt still runs".into());
    expected[1].outcome = Outcome::Kept(unnamed.into());
    expected[2].outcome = Outcome::Kept(foreign.into());
    assert_eq!(stopped, expected);
    let known = known.lock().unwrap();
    assert!(
        known.canceled().is_empty(),
        "canceled {:?}",
        known.canceled()
    );
    let asked: Vec<String> = known
        .requests
        .iter()
        .map(|(line, _)| task_id(line))
        .collect();
    assert!(asked.iter().all(|id| id == "live"), "asked about {asked:?}");
    for (root, text) in roots.iter().zip(before) {
        assert_eq!(
            std::fs::read_to_string(root.join("remote.json")).unwrap(),
            text
        );
    }
}

#[tokio::test]
async fn uses_journal_token() {
    let folder = tempfile::tempdir().unwrap();
    let known = Known::with(&[("running", "RUNNING")]);
    let address = serve(known.clone());
    let scratch = folder.path().join("scratch");
    let mine = backend(&address, Some("token-of-first"), &scratch);
    let theirs = backend(&address, Some("token-of-second"), &scratch);
    let root = leave(&mine, "first-sort-1-1", Some("first"), "running");
    let kept = theirs.stop_run("first", &[&theirs]).await.unwrap();
    assert!(matches!(
        kept[..],
        [Orphan {
            outcome: Outcome::Kept(_),
            ..
        }]
    ));
    assert!(known.lock().unwrap().requests.is_empty());
    let stopped = theirs.stop_run("first", &[&theirs, &mine]).await.unwrap();
    assert_eq!(
        stopped,
        [found(
            "first-sort-1-1",
            "running",
            Some("CANCELED"),
            Outcome::Canceled
        )]
    );
    assert!(!root.join("remote.json").exists());
    let known = known.lock().unwrap();
    assert_eq!(known.canceled(), ["running"]);
    let tokens: Vec<&str> = known
        .requests
        .iter()
        .map(|(_, token)| token.as_str())
        .collect();
    assert!(
        tokens.iter().all(|token| *token == "Bearer token-of-first"),
        "a request had another token"
    );
}
