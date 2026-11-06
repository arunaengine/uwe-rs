//! A fake of the Azure Batch REST API and the Entra ID and instance metadata token endpoints on
//! 127.0.0.1 for tests: it checks Shared Key signatures and runs each task's command here.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
#![allow(dead_code, reason = "each test file uses only a part of this module")]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;

/// The account of the fake and its made-up Shared Key.
pub const ACCOUNT: &str = "lab";
pub const KEY: &str = "dXdlLXRlc3Qta2V5LW5vdC1yZWFs";

/// A fake server, whose state the test reads and changes while it runs.
pub struct FakeAzure {
    /// The address of the server, like `http://127.0.0.1:4711`.
    pub address: String,
    state: Arc<Mutex<State>>,
}

/// One request: its method, path with query, lower-case headers and body.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    pub fn header(&self, name: &str) -> &str {
        self.headers.get(name).map_or("", String::as_str)
    }
}

#[derive(Default)]
struct State {
    folder: PathBuf,
    requests: Vec<Request>,
    pools: BTreeMap<String, Value>,
    jobs: BTreeMap<String, Value>,
    tasks: BTreeMap<String, Arc<Mutex<Task>>>,
    tokens: u32,
    /// How many job creations are still answered with the job quota error.
    quota: u32,
    /// The Shared Key signatures that did not match the request.
    unsigned: Vec<String>,
}

#[derive(Default)]
struct Task {
    folder: PathBuf,
    /// The process group of the command that runs.
    group: Option<u32>,
    exit_code: Option<i32>,
    terminated: bool,
}

impl FakeAzure {
    /// Starts a server that keeps the folders of tasks in `folder`.
    pub fn start(folder: &Path) -> FakeAzure {
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
        FakeAzure { address, state }
    }

    /// The requests so far whose method and path start with `prefix`, like `POST /jobs`.
    pub fn requests(&self, prefix: &str) -> Vec<Request> {
        let state = self.lock();
        let requests = (state.requests.iter())
            .filter(|request| format!("{} {}", request.method, request.path).starts_with(prefix));
        requests.cloned().collect()
    }

    /// Adds a pool that exists before the run, with its nodes.
    pub fn add_pool(&self, id: &str, slots: u32) {
        let pool = json!({"id": id, "state": "active", "allocationState": "steady", "vmSize": "standard_d4a_v4", "taskSlotsPerNode": slots, "currentDedicatedNodes": 1});
        self.lock().pools.insert(id.to_owned(), pool);
    }

    /// Takes the nodes of a pool away after a failed resize; its tasks wait.
    pub fn break_pool(&self, id: &str, code: &str) {
        let mut state = self.lock();
        let pool = state.pools.get_mut(id).expect("a pool");
        pool["currentDedicatedNodes"] = json!(0);
        pool["resizeErrors"] = json!([{"code": code, "message": "no VMs of this size"}]);
    }

    /// Answers the next `count` job creations with the job quota error.
    pub fn fill_quota(&self, count: u32) {
        self.lock().quota = count;
    }

    /// Whether the command of the task `id` runs.
    pub fn running(&self, id: &str) -> bool {
        let task = self.lock().tasks.get(id).cloned();
        task.is_some_and(|task| lock(&task).group.is_some())
    }

    /// The ids of the tasks so far, as `<job>/<task>`.
    pub fn tasks(&self) -> Vec<String> {
        self.lock().tasks.keys().cloned().collect()
    }

    /// The Shared Key requests whose signature the fake could not confirm.
    pub fn unsigned(&self) -> Vec<String> {
        self.lock().unsigned.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        lock(&self.state)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// An error answer of Azure Batch.
fn error(status: u16, code: &str) -> (u16, Value) {
    let message = json!({"lang": "en-US", "value": format!("{code}\nRequestId:1")});
    (status, json!({"code": code, "message": message}))
}

fn answer(state: &Mutex<State>, mut stream: TcpStream) {
    let Some((method, path, headers, body)) = read(&mut stream) else {
        return;
    };
    let request = Request {
        method: method.clone(),
        path: path.clone(),
        headers: headers.clone(),
        body: body.clone(),
    };
    let mut state = lock(state);
    state.requests.push(request.clone());
    if let Some(problem) = unsigned(&request) {
        state.unsigned.push(problem);
    }
    let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let (route, _) = path.split_once('?').unwrap_or((&path, ""));
    let parts: Vec<&str> = route.trim_start_matches('/').split('/').collect();
    let mut file = None;
    let (status, reply) = match (method.as_str(), parts.as_slice()) {
        ("POST", [_, "oauth2", "v2.0", "token"]) => {
            state.tokens += 1;
            (
                200,
                json!({"access_token": format!("token-{}", state.tokens), "expires_in": 3600}),
            )
        }
        ("GET", ["metadata", "identity", "oauth2", "token"])
            if headers.get("metadata").map(String::as_str) == Some("true") =>
        {
            (
                200,
                json!({"access_token": "vm-token", "expires_in": "3600"}),
            )
        }
        ("GET", ["supportedimages"]) => (
            200,
            json!({"value": [{"nodeAgentSKUId": "batch.node.ubuntu 24.04", "osType": "linux", "verificationType": "verified", "imageReference": {"publisher": "microsoft-dsvm", "offer": "ubuntu-hpc", "sku": "2404", "version": "latest"}}]}),
        ),
        ("GET", ["pools", id]) => match state.pools.get(*id) {
            Some(pool) => (200, pool.clone()),
            None => error(404, "PoolNotFound"),
        },
        ("POST", ["pools"]) => {
            let mut pool = json.clone();
            pool["state"] = json!("active");
            pool["allocationState"] = json!("steady");
            pool["currentDedicatedNodes"] = json!(1);
            let id = json["id"].as_str().unwrap_or_default().to_owned();
            state.pools.insert(id, pool);
            (201, Value::Null)
        }
        ("DELETE", ["pools", id]) => match state.pools.remove(*id) {
            Some(_) => (202, Value::Null),
            None => error(404, "PoolNotFound"),
        },
        ("POST", ["jobs"]) if state.quota > 0 => {
            state.quota -= 1;
            error(409, "ActiveJobAndScheduleQuotaReached")
        }
        ("POST", ["jobs"]) => {
            let id = json["id"].as_str().unwrap_or_default().to_owned();
            match state.jobs.contains_key(&id) {
                true => error(409, "JobExists"),
                false => {
                    state.jobs.insert(id, json.clone());
                    (201, Value::Null)
                }
            }
        }
        ("GET", ["jobs", id]) => match state.jobs.get(*id) {
            Some(job) => (200, job.clone()),
            None => error(404, "JobNotFound"),
        },
        ("PATCH", ["jobs", id]) => match state.jobs.get_mut(*id) {
            Some(job) => {
                job["onAllTasksComplete"] = json["onAllTasksComplete"].clone();
                (200, Value::Null)
            }
            None => error(404, "JobNotFound"),
        },
        ("DELETE", ["jobs", id]) => match state.jobs.remove(*id) {
            Some(_) => (202, Value::Null),
            None => error(404, "JobNotFound"),
        },
        ("POST", ["jobs", job, "tasks"]) if state.jobs.contains_key(*job) => {
            let key = format!("{job}/{}", json["id"].as_str().unwrap_or_default());
            let folder = state.folder.join(&key);
            let task = Arc::new(Mutex::new(Task {
                folder,
                ..Task::default()
            }));
            state.tasks.insert(key, task.clone());
            let pool = state.jobs[*job]["poolInfo"]["poolId"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let broken = state
                .pools
                .get(&pool)
                .is_none_or(|pool| pool["currentDedicatedNodes"] == 0);
            if !broken {
                std::thread::spawn(move || run(&task, &json));
            }
            (201, Value::Null)
        }
        ("GET", ["jobs", job, "tasks", id]) => match state.tasks.get(&format!("{job}/{id}")) {
            Some(task) => (200, describe(id, &lock(task))),
            None => error(404, "TaskNotFound"),
        },
        ("POST", ["jobs", job, "tasks", id, "terminate"]) => {
            match state.tasks.get(&format!("{job}/{id}")) {
                Some(task) => {
                    let mut task = lock(task);
                    task.terminated = true;
                    if let Some(group) = task.group {
                        let group = format!("-{group}");
                        let _ = Command::new("kill").args(["-KILL", "--", &group]).status();
                    }
                    (204, Value::Null)
                }
                None => error(404, "TaskNotFound"),
            }
        }
        ("DELETE", ["jobs", job, "tasks", id]) => {
            match state.tasks.remove(&format!("{job}/{id}")) {
                Some(_) => (200, Value::Null),
                None => error(404, "TaskNotFound"),
            }
        }
        ("GET" | "HEAD", ["jobs", job, "tasks", id, "files", name]) => {
            match state.tasks.get(&format!("{job}/{id}")) {
                Some(task) => {
                    let bytes = std::fs::read(lock(task).folder.join(name)).unwrap_or_default();
                    file = Some(range(&bytes, headers.get("ocp-range")));
                    (200, Value::Null)
                }
                None => error(404, "TaskNotFound"),
            }
        }
        _ => error(404, "UnknownCall"),
    };
    drop(state);
    let (kind, text) = match file {
        Some(bytes) => ("application/octet-stream", bytes),
        None if reply.is_null() => ("application/json", Vec::new()),
        None => ("application/json", reply.to_string().into_bytes()),
    };
    let reason = if status < 300 { "OK" } else { "Error" };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        text.len()
    );
    if method != "HEAD" {
        let _ = stream.write_all(&text);
    }
}

/// The bytes of a file in the range of an `ocp-range` header, like `bytes=10-19`.
fn range(bytes: &[u8], wanted: Option<&String>) -> Vec<u8> {
    let Some((start, end)) = wanted.and_then(|text| text.strip_prefix("bytes=")?.split_once('-'))
    else {
        return bytes.to_vec();
    };
    let start: usize = start.parse().unwrap_or(0);
    let end: usize = end
        .parse()
        .unwrap_or(bytes.len())
        .min(bytes.len().saturating_sub(1));
    bytes.get(start..=end).unwrap_or_default().to_vec()
}

/// Why the Shared Key signature of a request does not match it; `None` for a bearer token or a
/// signature that matches. The fake signs the request as Microsoft's Batch rules describe.
fn unsigned(request: &Request) -> Option<String> {
    let authorization = request.header("authorization");
    let signature = authorization.strip_prefix(&format!("SharedKey {ACCOUNT}:"))?;
    let (route, query) = request.path.split_once('?').unwrap_or((&request.path, ""));
    let mut text = format!("{}\n", request.method);
    for name in [
        "content-encoding",
        "content-language",
        "content-length",
        "content-md5",
        "content-type",
        "date",
        "if-modified-since",
        "if-match",
        "if-none-match",
        "if-unmodified-since",
        "range",
    ] {
        text.push_str(&format!("{}\n", request.header(name)));
    }
    for (name, value) in request
        .headers
        .iter()
        .filter(|(name, _)| name.starts_with("ocp-"))
    {
        text.push_str(&format!("{name}:{}\n", value.trim()));
    }
    text.push_str(&format!("/{ACCOUNT}{route}"));
    let mut pairs: Vec<(String, String)> = (query.split('&'))
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (name.to_lowercase(), value.replace("%20", " "))
        })
        .collect();
    pairs.sort();
    for (name, value) in pairs {
        text.push_str(&format!("\n{name}:{value}"));
    }
    let key = STANDARD.decode(KEY).expect("a Base64 key");
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&key).expect("a key");
    mac.update(text.as_bytes());
    let expected = STANDARD.encode(mac.finalize().into_bytes());
    (signature != expected).then(|| format!("{} {}: {text:?}", request.method, request.path))
}

/// Runs a task as a node would: downloads its resource files into its working folder, then runs
/// its command line there, with its output in `stdout.txt` and `stderr.txt`.
fn run(task: &Mutex<Task>, body: &Value) {
    let folder = lock(task).folder.clone();
    let working = folder.join("wd");
    std::fs::create_dir_all(&working).expect("makes the working folder");
    for file in body["resourceFiles"].as_array().into_iter().flatten() {
        let url = file["httpUrl"].as_str().unwrap_or_default();
        let path = working.join(file["filePath"].as_str().unwrap_or_default());
        std::fs::write(&path, download(url)).expect("writes a resource file");
        if file["fileMode"] == "0755" {
            let mode = std::os::unix::fs::PermissionsExt::from_mode(0o755);
            std::fs::set_permissions(&path, mode).expect("runs");
        }
    }
    let words = split(body["commandLine"].as_str().unwrap_or_default());
    let out = std::fs::File::create(folder.join("stdout.txt")).expect("the output");
    let err = std::fs::File::create(folder.join("stderr.txt")).expect("the errors");
    let mut command = Command::new(&words[0]);
    command.args(&words[1..]).current_dir(&working);
    for setting in body["environmentSettings"].as_array().into_iter().flatten() {
        let name = setting["name"].as_str().unwrap_or_default();
        command.env(name, setting["value"].as_str().unwrap_or_default());
    }
    use std::os::unix::process::CommandExt;
    let mut child = {
        let mut task = lock(task);
        if task.terminated {
