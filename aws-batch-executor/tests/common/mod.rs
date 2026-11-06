//! A fake of the AWS Batch and CloudWatch Logs APIs on 127.0.0.1 for tests: it keeps job
//! definitions, runs the command of each submitted job on this machine and serves its log.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
#![allow(dead_code, reason = "each test file uses only a part of this module")]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

/// A fake server, whose state the test reads and changes while it runs.
pub struct FakeBatch {
    /// The address of the server, like `http://127.0.0.1:4711`.
    pub address: String,
    state: Arc<Mutex<State>>,
}

/// One request: the Batch operation, like `submitjob`, or the target of CloudWatch Logs, the
/// `authorization` header, and the body.
#[derive(Clone, Debug)]
pub struct Request {
    pub operation: String,
    pub authorization: String,
    pub body: Value,
}

#[derive(Default)]
struct State {
    folder: PathBuf,
    requests: Vec<Request>,
    definitions: Vec<Value>,
    jobs: BTreeMap<String, Job>,
    /// How many `submitjob` calls are still answered with 429.
    throttled: u32,
    /// Whether no compute environment can run the jobs, which then never start.
    unschedulable: bool,
}

struct Job {
    child: Option<Child>,
    exit_code: Option<i32>,
    terminated: Option<String>,
    log: PathBuf,
}

impl FakeBatch {
    /// Starts a server that keeps the logs of jobs in `folder`.
    pub fn start(folder: &Path) -> FakeBatch {
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
        FakeBatch { address, state }
    }

    /// The requests of `operation` so far, like `submitjob` or `GetLogEvents`.
    pub fn requests(&self, operation: &str) -> Vec<Request> {
        let state = self.lock();
        let requests = state
            .requests
            .iter()
            .filter(|request| request.operation == operation);
        requests.cloned().collect()
    }

    /// Answers the next `count` submissions with 429, as a throttled account does.
    pub fn throttle(&self, count: u32) {
        self.lock().throttled = count;
    }

    /// Keeps all later jobs waiting, with the reason of a queue that cannot run them.
    pub fn unschedulable(&self) {
        self.lock().unschedulable = true;
    }

    /// Whether the job `id` runs.
    pub fn running(&self, id: &str) -> bool {
        let mut state = self.lock();
        let job = state.jobs.get_mut(id);
        job.is_some_and(|job| {
            job.child
                .as_mut()
                .is_some_and(|child| child.try_wait().ok().flatten().is_none())
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

fn answer(state: &Mutex<State>, mut stream: TcpStream) {
    let Some((path, headers, body)) = read(&mut stream) else {
        return;
    };
    let header = |name: &str| headers.get(name).cloned().unwrap_or_default();
    let operation = match path.strip_prefix("/v1/") {
        Some(operation) => operation.to_owned(),
        None => header("x-amz-target")
            .rsplit('.')
            .next()
            .unwrap_or_default()
            .to_owned(),
    };
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut state = state.lock().unwrap_or_else(|poison| poison.into_inner());
    state.requests.push(Request {
        operation: operation.clone(),
        authorization: header("authorization"),
        body: body.clone(),
    });
    let (status, reply) = match operation.as_str() {
        "describejobdefinitions" => {
            let name = &body["jobDefinitionName"];
            let found: Vec<Value> = (state.definitions.iter())
                .filter(|definition| &definition["jobDefinitionName"] == name)
                .cloned()
                .collect();
            (200, json!({"jobDefinitions": found}))
        }
        "registerjobdefinition" => {
            let name = body["jobDefinitionName"].clone();
            let revision = 1
                + (state.definitions.iter())
                    .filter(|definition| definition["jobDefinitionName"] == name)
                    .count();
            let mut kept = body.clone();
            kept["revision"] = json!(revision);
            kept["status"] = json!("ACTIVE");
            state.definitions.push(kept);
            (
                200,
                json!({"jobDefinitionName": name, "revision": revision}),
            )
        }
        "submitjob" if state.throttled > 0 => {
            state.throttled -= 1;
            (
                429,
                json!({"__type": "TooManyRequestsException", "message": "Too Many Requests"}),
            )
        }
        "submitjob" => {
            let id = format!("job-{}", state.jobs.len() + 1);
            let log = state.folder.join(format!("{id}.log"));
            let command: Vec<String> = (body["containerOverrides"]["command"].as_array())
                .map(|words| {
                    words
                        .iter()
                        .filter_map(|word| word.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let child = match state.unschedulable {
                true => None,
                false => Some(start(&command, &log)),
            };
            let job = Job {
                child,
                exit_code: None,
                terminated: None,
                log,
            };
            state.jobs.insert(id.clone(), job);
            (200, json!({"jobId": id, "jobName": body["jobName"]}))
        }
        "describejobs" => {
            let unschedulable = state.unschedulable;
            let ids: Vec<String> = (body["jobs"].as_array().into_iter().flatten())
                .filter_map(|id| id.as_str().map(str::to_owned))
                .collect();
            let mut jobs = Vec::new();
            for id in &ids {
                if let Some(job) = state.jobs.get_mut(id) {
                    jobs.push(describe(id, job, unschedulable));
                }
            }
            (200, json!({"jobs": jobs}))
        }
        "terminatejob" => {
            let id = body["jobId"].as_str().unwrap_or_default();
            if let Some(job) = state.jobs.get_mut(id) {
                // The job is a process group, as a container holds all processes of its task.
                if let Some(child) = &mut job.child {
                    let group = format!("-{}", child.id());
                    let _ = Command::new("kill").args(["-KILL", "--", &group]).status();
                    let _ = child.wait();
                }
                job.terminated = Some(body["reason"].as_str().unwrap_or_default().to_owned());
            }
            (200, json!({}))
        }
        "GetLogEvents" => {
            let stream = body["logStreamName"].as_str().unwrap_or_default();
            let id = stream.rsplit('/').next().unwrap_or_default();
            match state.jobs.get(id) {
                None => (
                    400,
                    json!({"__type": "ResourceNotFoundException", "message": "The specified log stream does not exist."}),
                ),
                Some(job) => {
                    let text = std::fs::read_to_string(&job.log).unwrap_or_default();
                    let events: Vec<Value> = (text.lines())
                        .map(|line| json!({"timestamp": 0, "message": line}))
                        .collect();
                    (200, json!({"events": events}))
                }
            }
        }
        _ => (404, json!({"message": "unknown operation"})),
    };
    let text = reply.to_string();
    let reason = if status == 200 { "OK" } else { "Error" };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{text}",
        text.len()
    );
}

/// Runs a job's command, as its container would, with its output in `log`.
fn start(command: &[String], log: &Path) -> Child {
    let out = std::fs::File::create(log).expect("makes the log");
    let err = out.try_clone().expect("a second handle");
    use std::os::unix::process::CommandExt;
    Command::new(&command[0])
        .args(&command[1..])
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .expect("starts the command of the job")
}

fn describe(id: &str, job: &mut Job, unschedulable: bool) -> Value {
    if job.exit_code.is_none()
        && let Some(child) = &mut job.child
        && let Ok(Some(status)) = child.try_wait()
    {
        use std::os::unix::process::ExitStatusExt;
        job.exit_code = Some(status.code().unwrap_or(128 + status.signal().unwrap_or(9)));
    }
    let stream = format!("uwe/default/{id}");
    match (job.exit_code, &job.terminated) {
        (_, Some(reason)) => json!({"jobId": id, "status": "FAILED", "statusReason": reason}),
        (None, None) if unschedulable => json!({
            "jobId": id,
            "status": "RUNNABLE",
            "statusReason": "MISCONFIGURATION:JOB_RESOURCE_REQUIREMENT - The job resource requirement (vCPU/memory/GPU) is higher than that can be met by the CE(s) attached to the job queue.",
        }),
        (None, None) => {
            json!({"jobId": id, "status": "RUNNING", "container": {"logStreamName": stream}})
        }
        (Some(code), None) => json!({
            "jobId": id,
            "status": if code == 0 { "SUCCEEDED" } else { "FAILED" },
            "statusReason": "Essential container in task exited",
            "container": {"exitCode": code, "logStreamName": stream},
            "attempts": [{"container": {"exitCode": code, "logStreamName": stream}}],
        }),
    }
}

/// The path, the lower-case headers and the body of one request.
fn read(stream: &mut TcpStream) -> Option<(String, BTreeMap<String, String>, Vec<u8>)> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut first = String::new();
    reader.read_line(&mut first).ok()?;
    let path = first.split_whitespace().nth(1)?.to_owned();
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
    Some((path, headers, body))
}
