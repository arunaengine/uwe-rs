//! A fake of the Nomad HTTP API on 127.0.0.1 for tests: it runs the command of each registered
//! job on this machine, after it fetched its artifacts, and serves its allocation and log.
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
use sha2::{Digest, Sha256};

/// A fake server, whose state the test reads and changes while it runs.
pub struct FakeNomad {
    /// The address of the server, like `http://127.0.0.1:4711`.
    pub address: String,
    state: Arc<Mutex<State>>,
}

/// One request: its method, path with query, `X-Nomad-Token` header and body.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub token: String,
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
    /// Whether no node fits the jobs registered from now on.
    unplaceable: bool,
}

#[derive(Default)]
struct Job {
    placed: bool,
    /// The process group of the command that runs.
    group: Option<u32>,
    exit_code: Option<i32>,
    stopped: bool,
    purged: bool,
    folder: PathBuf,
}

impl FakeNomad {
    /// Starts a server that keeps the folders of jobs in `folder`.
    pub fn start(folder: &Path) -> FakeNomad {
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
        FakeNomad { address, state }
    }

    /// The requests so far whose method and path start with `prefix`, like `POST /v1/jobs`.
    pub fn requests(&self, prefix: &str) -> Vec<Request> {
        let state = self.lock();
        let requests = (state.requests.iter())
            .filter(|request| format!("{} {}", request.method, request.path).starts_with(prefix));
        requests.cloned().collect()
    }

    /// Lets no node take the jobs registered from now on.
    pub fn unplaceable(&self) {
        self.lock().unplaceable = true;
    }

    /// Whether the command of the job `id` runs.
    pub fn running(&self, id: &str) -> bool {
        let job = self.lock().jobs.get(id).cloned();
        job.is_some_and(|job| lock(&job).group.is_some())
    }

    /// Whether the job `id` was purged.
    pub fn purged(&self, id: &str) -> bool {
        let job = self.lock().jobs.get(id).cloned();
        job.is_some_and(|job| lock(&job).purged)
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
        token: headers.get("x-nomad-token").cloned().unwrap_or_default(),
        body: body.clone(),
    };
    let mut state = lock(state);
    state.requests.push(request);
    let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let route = path.split('?').next().unwrap_or_default().to_owned();
    let parts: Vec<&str> = route.trim_start_matches('/').split('/').collect();
    let (status, reply) = match (method.as_str(), parts.as_slice()) {
        ("POST", ["v1", "jobs"]) => {
            let id = json["Job"]["ID"].as_str().unwrap_or_default().to_owned();
            let job = Arc::new(Mutex::new(Job {
                placed: !state.unplaceable,
                folder: state.folder.join(&id),
                ..Job::default()
            }));
            state.jobs.insert(id.clone(), job.clone());
            if !state.unplaceable {
                std::thread::spawn(move || run(&job, &json["Job"]));
            }
            (
                200,
                json!({"EvalID": format!("eval-{id}"), "JobModifyIndex": 1}),
            )
        }
        ("GET", ["v1", "job", id, "allocations"]) => match state.jobs.get(*id) {
            Some(job) => (200, allocations(id, &lock(job))),
            None => (404, json!("job not found")),
        },
        ("GET", ["v1", "job", id, "evaluations"]) => match state.jobs.get(*id) {
            Some(job) if !lock(job).placed => {
                let metrics = json!({"NodesEvaluated": 2, "NodesFiltered": 2,
                                     "ConstraintFiltered": {"${node.pool} = gpu": 2}});
                let failed = json!({"uwe": metrics});
                (
                    200,
                    json!([{"ID": "eval-1", "Status": "blocked", "ModifyIndex": 2,
                              "FailedTGAllocs": failed}]),
                )
            }
            Some(_) => (
                200,
                json!([{"ID": "eval-1", "Status": "complete", "ModifyIndex": 2}]),
            ),
            None => (404, json!("job not found")),
        },
        ("DELETE", ["v1", "job", id]) => match state.jobs.get(*id) {
            Some(job) => {
                let mut job = lock(job);
                job.stopped = true;
                job.purged |= path.contains("purge=true");
                if let Some(group) = job.group {
                    let group = format!("-{group}");
                    let _ = Command::new("kill").args(["-KILL", "--", &group]).status();
                }
                (200, json!({"EvalID": "eval-stop"}))
            }
            None => (404, json!("job not found")),
        },
        ("GET", ["v1", "client", "fs", "logs", alloc]) => {
            let id = alloc.trim_start_matches("alloc-");
            let text = (state.jobs.get(id))
                .map(|job| std::fs::read_to_string(lock(job).folder.join("stderr")))
                .and_then(Result::ok)
                .unwrap_or_default();
            drop(state);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{text}",
                text.len()
            );
            return;
        }
        _ => (404, json!("unknown call")),
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

/// Fetches the artifacts of a job and checks them, then runs its command in its start folder,
/// as a Nomad client with the docker driver would, with its output in the folder of the job.
fn run(job: &Mutex<Job>, body: &Value) {
    let task = &body["TaskGroups"][0]["Tasks"][0];
    let folder = lock(job).folder.clone();
    let local = folder.join("local");
    std::fs::create_dir_all(&local).expect("makes the task folder");
    for artifact in task["Artifacts"].as_array().into_iter().flatten() {
        let source = artifact["GetterSource"].as_str().unwrap_or_default();
        let path = source
            .strip_prefix("file://")
            .expect("a file:// artifact in tests");
        let bytes = std::fs::read(percent_decoded(path)).expect("reads the artifact");
        let checksum = artifact["GetterOptions"]["checksum"]
            .as_str()
            .unwrap_or_default();
        let digest: String = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if checksum != format!("sha256:{digest}") {
            lock(job).exit_code = Some(1);
            return;
        }
        let target = folder.join(artifact["RelativeDest"].as_str().unwrap_or_default());
        std::fs::write(target, bytes).expect("writes the artifact");
    }
    let config = &task["Config"];
    let mut words = vec![config["command"].as_str().unwrap_or_default().to_owned()];
    let args = config["args"].as_array().into_iter().flatten();
    words.extend(args.filter_map(|word| word.as_str().map(str::to_owned)));
    let start = (config["work_dir"].as_str()).map_or(folder.clone(), PathBuf::from);
    let out = std::fs::File::create(folder.join("stdout")).expect("opens the log");
    let err = std::fs::File::create(folder.join("stderr")).expect("opens the log");
    let environment = task["Env"].as_object().cloned().unwrap_or_default();
    use std::os::unix::process::CommandExt;
    let mut child = {
        let mut job = lock(job);
        if job.stopped {
            return;
        }
        let mut command = Command::new(&words[0]);
        command
            .args(&words[1..])
            .current_dir(start)
            .env("NOMAD_TASK_DIR", &local)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err);
        for (name, value) in environment {
            command.env(name, value.as_str().unwrap_or_default());
        }
        let child = command.spawn().expect("starts the command");
        job.group = Some(child.id());
        child
    };
    let status = child.wait().expect("the command ends");
    use std::os::unix::process::ExitStatusExt;
    let mut job = lock(job);
    job.group = None;
    job.exit_code = Some(status.code().unwrap_or(128 + status.signal().unwrap_or(9)));
}

fn allocations(id: &str, job: &Job) -> Value {
    if !job.placed {
        return json!([]);
    }
    let (status, state, events) = match (job.stopped, job.exit_code) {
        (true, _) => (
            "complete",
            "dead",
            json!([{"Type": "Killed", "ExitCode": 0}]),
        ),
        (false, None) => (
            "running",
            "running",
            json!([{"Type": "Started", "ExitCode": 0}]),
        ),
        (false, Some(code)) => {
            let status = if code == 0 { "complete" } else { "failed" };
            let events = json!([
                {"Type": "Started", "ExitCode": 0},
                {"Type": "Terminated", "ExitCode": code,
                 "DisplayMessage": format!("Exit Code: {code}")},
            ]);
            (status, "dead", events)
        }
    };
    let failed = status == "failed";
    json!([{
        "ID": format!("alloc-{id}"), "JobID": id, "NodeID": "node-1", "NodeName": "fake",
        "ClientStatus": status, "CreateIndex": 1,
        "TaskStates": {"task": {"State": state, "Failed": failed, "Events": events}},
    }])
}

/// A path of a `file://` URL with its `%XX` escapes decoded.
fn percent_decoded(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut decoded = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                decoded.push(byte);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
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
