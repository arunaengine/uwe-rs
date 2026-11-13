//! A fake of the Google Batch, OAuth token, metadata and Cloud Logging APIs on 127.0.0.1 for
//! tests: it runs the runnables of each created job on this machine and serves their log.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
#![allow(dead_code, reason = "each test file uses only a part of this module")]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

/// A fake server, whose state the test reads and changes while it runs.
pub struct FakeGoogle {
    /// The address of the server, like `http://127.0.0.1:4711`.
    pub address: String,
    state: Arc<Mutex<State>>,
}

/// One request: its method, path with query, `authorization` header and body.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub authorization: String,
    pub body: Vec<u8>,
}

impl Request {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

#[derive(Default)]
struct State {
    folder: PathBuf,
    requests: Vec<Request>,
    jobs: BTreeMap<String, Arc<Mutex<Job>>>,
    tokens: u32,
    /// How many job creations are still answered with 429.
    throttled: u32,
    /// The exit code that every later task ends with instead of its own, like 50001.
    reserved: Option<i32>,
}

#[derive(Default)]
struct Job {
    uid: String,
    /// The process group of the runnable that runs.
    group: Option<u32>,
    exit_code: Option<i32>,
    deleted: bool,
    log: PathBuf,
}

impl FakeGoogle {
    /// Starts a server that keeps the logs of jobs in `folder`.
    pub fn start(folder: &Path) -> FakeGoogle {
        std::fs::create_dir_all(folder).expect("makes the folder of the fake");
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        let address = format!("http://{}", listener.local_addr().expect("an address"));
        let state = Arc::new(Mutex::new(State {
            folder: folder.to_owned(),
            ..State::default()
        }));
        let shared = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let state = shared.clone();
                std::thread::spawn(move || answer(&state, stream));
            }
        });
        FakeGoogle { address, state }
    }

    /// The requests so far whose method and path start with `prefix`, like `POST /token`.
    pub fn requests(&self, prefix: &str) -> Vec<Request> {
        let state = self.lock();
        let requests = (state.requests.iter())
            .filter(|request| format!("{} {}", request.method, request.path).starts_with(prefix));
        requests.cloned().collect()
    }

    /// Answers the next `count` job creations with 429, as a busy project does.
    pub fn throttle(&self, count: u32) {
        self.lock().throttled = count;
    }

    /// Ends every later task with `code`, as a VM that Google ends does.
    pub fn end_with(&self, code: i32) {
        self.lock().reserved = Some(code);
    }

    /// Whether a runnable of the job `id` runs.
    pub fn running(&self, id: &str) -> bool {
        let job = self.lock().jobs.get(id).cloned();
        job.is_some_and(|job| lock(&job).group.is_some())
    }

    /// The ids of the jobs so far.
    pub fn jobs(&self) -> Vec<String> {
        self.lock().jobs.keys().cloned().collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        lock(&self.state)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn answer(state: &Mutex<State>, mut stream: TcpStream) {
    let Some((method, path, headers, body)) = read(&mut stream) else {
        return;
    };
    let request = Request {
        method: method.clone(),
        path: path.clone(),
        authorization: headers.get("authorization").cloned().unwrap_or_default(),
        body: body.clone(),
    };
    let mut state = lock(state);
    state.requests.push(request);
    let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let jobs = path.split_once("/jobs").map(|(_, rest)| rest.to_owned());
    let (status, reply) = match (method.as_str(), path.as_str(), jobs.as_deref()) {
        ("POST", "/token", _) => {
            state.tokens += 1;
            let token = format!("token-{}", state.tokens);
            (200, json!({"access_token": token, "expires_in": 3600}))
        }
        ("GET", "/computeMetadata/v1/instance/service-accounts/default/token", _)
            if headers.get("metadata-flavor").map(String::as_str) == Some("Google") =>
        {
            (200, json!({"access_token": "vm-token", "expires_in": 3600}))
        }
        ("POST", _, Some(query)) if state.throttled > 0 && query.starts_with("?job_id=") => {
            state.throttled -= 1;
            let error = json!({"code": 429, "status": "RESOURCE_EXHAUSTED", "message": "Quota"});
            (429, json!({"error": error}))
        }
        ("POST", _, Some(query)) if query.starts_with("?job_id=") => {
            let id = query.trim_start_matches("?job_id=").to_owned();
            let uid = format!("uid-{}", state.jobs.len() + 1);
            let job = Arc::new(Mutex::new(Job {
                uid: uid.clone(),
                log: state.folder.join(format!("{id}.log")),
                ..Job::default()
            }));
            state.jobs.insert(id.clone(), job.clone());
            let reserved = state.reserved;
            std::thread::spawn(move || run(&job, &json, reserved));
            (200, json!({"name": id, "uid": uid}))
        }
        (method, _, Some(id)) if id.starts_with('/') => {
            let id = id.trim_start_matches('/');
            match (state.jobs.get(id), method) {
                (None, _) => {
                    let error = json!({"code": 404, "status": "NOT_FOUND", "message": "no job"});
                    (404, json!({"error": error}))
                }
                (Some(job), "DELETE") => {
                    let mut job = lock(job);
                    job.deleted = true;
                    if let Some(group) = job.group {
                        let group = format!("-{group}");
                        let _ = Command::new("kill").args(["-KILL", "--", &group]).status();
                    }
                    (200, json!({}))
                }
                (Some(job), _) => (200, describe(id, &lock(job))),
            }
        }
        ("POST", "/v2/entries:list", _) => {
            let filter = json["filter"].as_str().unwrap_or_default();
            let uid = filter
                .rsplit("labels.job_uid=\"")
                .next()
                .unwrap_or_default();
            let uid = uid.trim_end_matches('"');
            let job = (state.jobs.values()).find(|job| lock(job).uid == uid);
            let text = job.map(|job| std::fs::read_to_string(&lock(job).log).unwrap_or_default());
            let lines = text.unwrap_or_default();
            let entries: Vec<Value> = (lines.lines().rev())
                .map(|line| json!({"textPayload": line, "severity": "INFO"}))
                .collect();
            (200, json!({"entries": entries}))
        }
        _ => (
            404,
            json!({"error": {"code": 404, "message": "unknown call"}}),
        ),
    };
    drop(state);
    let text = reply.to_string();
    let reason = if status == 200 { "OK" } else { "Error" };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{text}",
        text.len()
    );
}

/// Runs the runnables of a job in order, as its VM would, with their output in its log.
fn run(job: &Mutex<Job>, body: &Value, reserved: Option<i32>) {
    let runnables = body["taskGroups"][0]["taskSpec"]["runnables"].as_array();
    let log = lock(job).log.clone();
    let mut code = 0;
    for runnable in runnables.into_iter().flatten() {
        let container = &runnable["container"];
        let mut words: Vec<String> = (container["entrypoint"].as_str())
            .filter(|entrypoint| !entrypoint.is_empty())
            .map(str::to_owned)
            .into_iter()
            .collect();
        let commands = container["commands"].as_array().into_iter().flatten();
        words.extend(commands.filter_map(|word| word.as_str().map(str::to_owned)));
        let out = (std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log))
        .expect("opens the log");
        let err = out.try_clone().expect("a second handle");
        use std::os::unix::process::CommandExt;
        let mut child = {
            let mut job = lock(job);
            if job.deleted {
                return;
            }
            let child = Command::new(&words[0])
                .args(&words[1..])
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(out)
                .stderr(err)
                .spawn()
                .expect("starts a runnable");
            job.group = Some(child.id());
            child
        };
        let status = child.wait().expect("the runnable ends");
        use std::os::unix::process::ExitStatusExt;
        code = status.code().unwrap_or(128 + status.signal().unwrap_or(9));
        lock(job).group = None;
        if code != 0 {
            break;
        }
    }
    lock(job).exit_code = Some(reserved.unwrap_or(code));
}

fn describe(id: &str, job: &Job) -> Value {
    let (state, events) = match (job.deleted, job.exit_code) {
        (true, _) => ("DELETION_IN_PROGRESS", json!([])),
        (false, None) => ("RUNNING", json!([])),
        (false, Some(code)) => {
            let state = if code == 0 { "SUCCEEDED" } else { "FAILED" };
            let events = json!([
                {"type": "STATUS_CHANGED", "eventTime": "2026-11-06T10:00:00Z",
                 "description": "Job state is set from SCHEDULED to RUNNING"},
                {"type": "STATUS_CHANGED", "eventTime": "2026-11-06T10:00:01.25Z",
                 "description": format!("Task state is updated from RUNNING to {state} on zones/europe-west4-a/instances/1 with exit code {code}."),
                 "taskExecution": {"exitCode": code}},
            ]);
            (state, events)
        }
    };
    json!({"name": id, "uid": job.uid, "status": {"state": state, "statusEvents": events}})
}

/// The method, the path with its query, the lower-case headers and the body of one request.
#[expect(clippy::type_complexity, reason = "the parts of one HTTP request")]
fn read(stream: &mut TcpStream) -> Option<(String, String, BTreeMap<String, String>, Vec<u8>)> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut first = String::new();
    reader.read_line(&mut first).ok()?;
    let mut words = first.split_whitespace();
    let (method, path) = (words.next()?.to_owned(), words.next()?.to_owned());
    let mut headers = BTreeMap::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        if line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let length: usize = headers
        .get("content-length")
        .and_then(|text| text.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    Some((method, path, headers, body))
}
