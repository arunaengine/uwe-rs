//! A fake of the JARVICE API on 127.0.0.1 for tests: it runs the decoded job script of each batch
//! job on this machine and serves its status and output, as `jarvice_fake.py` of the spec does.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
#![allow(dead_code, reason = "each test file uses only a part of this module")]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use base64::Engine;
use serde_json::{Value, json};

/// The header that JARVICE puts before the output of a job.
const HEADER: &str = "JARVICE job output\n\
    ###############################################################################\n";

/// A fake server, whose state the test reads while it runs.
pub struct FakeJarvice {
    /// The address of the API, like `http://127.0.0.1:4711`.
    pub address: String,
    state: Arc<Mutex<State>>,
}

/// One request: its method, its path with the query, and its body, with the API key masked.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub body: Value,
}

#[derive(Default)]
struct State {
    requests: Vec<Request>,
    jobs: BTreeMap<u64, Arc<Mutex<Job>>>,
}

#[derive(Default)]
struct Job {
    /// The process group of the job script while it runs.
    group: Option<u32>,
    exit_code: Option<i32>,
    stopped: bool,
    output: String,
}

impl FakeJarvice {
    pub fn start() -> FakeJarvice {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        let address = format!("http://{}", listener.local_addr().expect("an address"));
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let state = shared.clone();
                std::thread::spawn(move || answer(&state, stream));
            }
        });
        FakeJarvice { address, state }
    }

    /// The requests so far whose method and path start with `prefix`, like `POST /jarvice/batch`.
    pub fn requests(&self, prefix: &str) -> Vec<Request> {
        let state = lock(&self.state);
        let requests = (state.requests.iter())
            .filter(|request| format!("{} {}", request.method, request.path).starts_with(prefix));
        requests.cloned().collect()
    }

    /// Whether the job script of the job `number` runs.
    pub fn running(&self, number: u64) -> bool {
        let job = lock(&self.state).jobs.get(&number).cloned();
        job.is_some_and(|job| lock(&job).group.is_some())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn answer(state: &Mutex<State>, mut stream: TcpStream) {
    let Some((method, path, body)) = read(&mut stream) else {
        return;
    };
    let mut body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let script = body["container"]["jobscript"].as_str().map(|text| {
        let bytes = base64::engine::general_purpose::STANDARD.decode(text);
        String::from_utf8_lossy(&bytes.unwrap_or_default()).into_owned()
    });
    if body["user"]["apikey"].is_string() {
        body["user"]["apikey"] = json!("<masked>");
    }
    let (route, query) = path.split_once('?').unwrap_or((&path, ""));
    let query: BTreeMap<&str, &str> = (query.split('&'))
        .filter_map(|pair| pair.split_once('='))
        .collect();
    let masked = match query.contains_key("apikey") {
        true => path.replace(query["apikey"], "<masked>"),
        false => path.clone(),
    };
    let mut state = lock(state);
    state.requests.push(Request {
        method: method.clone(),
        path: masked,
        body,
    });
    let number = (query.get("name").and_then(|name| name.rsplit('-').next()))
        .and_then(|number| number.parse::<u64>().ok());
    let job = number.and_then(|number| state.jobs.get(&number).cloned());
    let (status, reply) = match (method.as_str(), route, job) {
        ("POST", "/jarvice/batch", _) => {
            let number = 9001 + state.jobs.len() as u64;
            let job = Arc::new(Mutex::new(Job::default()));
            state.jobs.insert(number, job.clone());
            let script = script.unwrap_or_default();
            std::thread::spawn(move || run(&job, &script));
            let name = format!("20261106120000-FAKE-jarvice-batch-{number}");
            (200, json!({"name": name, "number": number}))
        }
        ("GET", "/jarvice/status", Some(job)) => {
            let job = lock(&job);
            let status = match (job.stopped, job.exit_code) {
                (true, _) => "TERMINATED",
                (false, None) => "PROCESSING STARTING",
                (false, Some(0)) => "COMPLETED",
                (false, Some(_)) => "COMPLETED WITH ERROR",
            };
            (
                200,
                json!({number.unwrap_or_default().to_string(): {"job_status": status}}),
            )
        }
        ("GET", "/jarvice/shutdown" | "/jarvice/terminate", Some(job)) => {
            let mut job = lock(&job);
            job.stopped = true;
            if let Some(group) = job.group {
                let group = format!("-{group}");
                let _ = Command::new("kill").args(["-KILL", "--", &group]).status();
            }
            (200, json!({"status": "shutdown requested"}))
        }
        ("GET", "/jarvice/output", Some(job)) => {
            let text = format!("{HEADER}{}", lock(&job).output);
            drop(state);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{text}",
                text.len()
            );
            return;
        }
        (_, _, None) if route != "/jarvice/batch" => (200, json!({"error": "Job not found"})),
        _ => (404, json!({"error": "unknown call"})),
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

/// Runs the job script with `bash`, as the container of a JARVICE job would, and keeps its
/// output and exit code.
fn run(job: &Mutex<Job>, script: &str) {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    let child = {
        let mut job = lock(job);
        let child = Command::new("bash")
            .args(["-c", script])
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("starts the job script");
        job.group = Some(child.id());
        child
    };
    let output = child.wait_with_output().expect("the job script ends");
    let mut job = lock(job);
    job.group = None;
    job.output = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let status = output.status;
    job.exit_code = Some(status.code().unwrap_or(128 + status.signal().unwrap_or(9)));
}

/// The method, the path with its query, and the body of one request.
fn read(stream: &mut TcpStream) -> Option<(String, String, Vec<u8>)> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut first = String::new();
    reader.read_line(&mut first).ok()?;
    let mut words = first.split_whitespace();
    let (method, path) = (words.next()?.to_owned(), words.next()?.to_owned());
    let mut length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        if line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    Some((method, path, body))
}
