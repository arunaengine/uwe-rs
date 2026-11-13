//! A fake of Flyte's Connect API on 127.0.0.1 for tests: auth metadata, a token endpoint, and
//! runs whose container arguments run here after the helper input was downloaded.
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
use serde_json::{Value, json};

/// The client of the API key that the fake accepts.
pub const CLIENT: &str = "uwe-app";
pub const SECRET: &str = "not-a-real-secret";

/// A fake server, whose state the test reads and changes while it runs.
pub struct FakeFlyte {
    /// The address of the server, like `http://127.0.0.1:4711`.
    pub address: String,
    state: Arc<Mutex<State>>,
}

/// One request: its method path, its `flyte-authorization` header and body.
#[derive(Clone, Debug)]
pub struct Request {
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
    runs: BTreeMap<String, Arc<Mutex<Run>>>,
    /// The number of the token that calls must carry; older ones are refused.
    token: u32,
}

#[derive(Default)]
struct Run {
    /// The process group of the container that runs.
    group: Option<u32>,
    started: bool,
    exit_code: Option<i32>,
    aborted: bool,
    folder: PathBuf,
}

impl FakeFlyte {
    /// Starts a server that keeps the folders of runs in `folder`.
    pub fn start(folder: &Path) -> FakeFlyte {
        std::fs::create_dir_all(folder).expect("makes the folder of the fake");
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        let address = format!("http://{}", listener.local_addr().expect("an address"));
        let state = Arc::new(Mutex::new(State {
            folder: folder.to_owned(),
            token: 1,
            ..State::default()
        }));
        let (shared, base) = (state.clone(), address.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (state, base) = (shared.clone(), base.clone());
                std::thread::spawn(move || answer(&state, &base, stream));
            }
        });
        FakeFlyte { address, state }
    }

    /// An API key for this server, as `FLYTE_API_KEY` holds it.
    pub fn api_key(&self, org: &str) -> String {
        STANDARD.encode(format!("{}:{CLIENT}:{SECRET}:{org}", self.address))
    }

    /// The requests so far whose path starts with `prefix`, like `/flyteidl2.workflow`.
    pub fn requests(&self, prefix: &str) -> Vec<Request> {
        let state = self.lock();
        let requests = state
            .requests
            .iter()
            .filter(|request| request.path.starts_with(prefix));
        requests.cloned().collect()
    }

    /// Refuses the tokens given so far, as when they end early.
    pub fn rotate(&self) {
        self.lock().token += 1;
    }

    /// Whether the container of the run `name` runs.
    pub fn running(&self, name: &str) -> bool {
        let run = self.lock().runs.get(name).cloned();
        run.is_some_and(|run| lock(&run).group.is_some())
    }

    /// The names of the runs so far.
    pub fn runs(&self) -> Vec<String> {
        self.lock().runs.keys().cloned().collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        lock(&self.state)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn answer(state: &Mutex<State>, base: &str, mut stream: TcpStream) {
    let Some((path, headers, body)) = read(&mut stream) else {
        return;
    };
    let token = headers
        .get("flyte-authorization")
        .cloned()
        .unwrap_or_default();
    let mut state = lock(state);
    state.requests.push(Request {
        path: path.clone(),
        token: token.clone(),
        body: body.clone(),
    });
    let method = path.trim_start_matches('/');
    if method == "oauth2/token" {
        let basic = format!("Basic {}", STANDARD.encode(format!("{CLIENT}:{SECRET}")));
        let form = String::from_utf8_lossy(&body);
        let granted = headers.get("authorization") == Some(&basic)
            && form.contains("grant_type=client_credentials");
        let reply = match granted {
            true => json!({"access_token": format!("token-{}", state.token),
                           "token_type": "bearer", "expires_in": 3600}),
            false => json!({"error": "invalid_client"}),
        };
        drop(state);
        return send(&mut stream, if granted { 200 } else { 401 }, &reply);
    }
    if method.starts_with("flyteidl2.auth.") {
        let reply = match method.ends_with("GetPublicClientConfig") {
            true => {
                json!({"clientId": "flytectl", "authorizationMetadataKey": "flyte-authorization",
                           "scopes": ["all"]})
            }
            false => json!({"issuer": base, "tokenEndpoint": format!("{base}/oauth2/token")}),
        };
        drop(state);
        return send(&mut stream, 200, &reply);
    }
    if token != format!("Bearer token-{}", state.token) {
        drop(state);
        let reply = json!({"code": "unauthenticated", "message": "the token is not valid"});
        return send(&mut stream, 401, &reply);
    }
    if method == "flyteidl2.workflow.RunLogsService/TailLogs" {
        let request: Value =
            serde_json::from_slice(body.get(5..).unwrap_or_default()).unwrap_or_default();
        let name = request["actionId"]["run"]["name"]
            .as_str()
            .unwrap_or_default();
        let folder = (state.runs.get(name)).map(|run| lock(run).folder.clone());
        drop(state);
        return send_log(&mut stream, folder);
    }
    let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let name = json["runId"]["name"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let (status, reply) = match method {
        "flyteidl2.workflow.RunService/CreateRun" if state.runs.contains_key(&name) => (
            409,
            json!({"code": "already_exists", "message": "the run exists"}),
        ),
        "flyteidl2.workflow.RunService/CreateRun" => {
            let run = Arc::new(Mutex::new(Run {
                folder: state.folder.join(&name),
                ..Run::default()
            }));
            state.runs.insert(name.clone(), run.clone());
            std::thread::spawn(move || start(&run, &json));
            (200, json!({"run": {"action": {"id": {"name": "a0"}}}}))
        }
        "flyteidl2.workflow.RunService/GetRunDetails" => match state.runs.get(&name) {
            Some(run) => (200, details(&lock(run))),
            None => (404, json!({"code": "not_found", "message": "no such run"})),
        },
        "flyteidl2.workflow.RunService/AbortRun" => match state.runs.get(&name) {
            Some(run) => {
                let mut run = lock(run);
                run.aborted = true;
                if let Some(group) = run.group {
                    let group = format!("-{group}");
                    let _ = Command::new("kill").args(["-KILL", "--", &group]).status();
                }
                (200, json!({}))
            }
            None => (404, json!({"code": "not_found", "message": "no such run"})),
        },
        _ => (
            404,
            json!({"code": "unimplemented", "message": "unknown call"}),
        ),
    };
    drop(state);
    send(&mut stream, status, &reply);
}

/// Downloads the blob inputs of a run to its input folder, as Flyte's copilot does, then runs
/// its container arguments here, with its output in the folder of the run.
fn start(run: &Mutex<Run>, body: &Value) {
    let template = &body["taskSpec"]["taskTemplate"];
    let container = &template["container"];
    let folder = lock(run).folder.clone();
    std::fs::create_dir_all(&folder).expect("makes the folder of the run");
    let inputs = PathBuf::from(
        container["dataConfig"]["inputPath"]
            .as_str()
            .unwrap_or("/none"),
    );
    for literal in body["inputs"]["literals"].as_array().into_iter().flatten() {
        let uri = literal["value"]["scalar"]["blob"]["uri"]
            .as_str()
            .unwrap_or_default();
        let source = uri.strip_prefix("file://").unwrap_or(uri);
        std::fs::create_dir_all(&inputs).expect("makes the input folder");
        let name = literal["name"].as_str().unwrap_or_default();
        std::fs::copy(source, inputs.join(name)).expect("downloads the blob");
    }
    let words: Vec<String> = (container["args"].as_array().into_iter().flatten())
        .filter_map(|word| word.as_str().map(str::to_owned))
        .collect();
    let out = std::fs::File::create(folder.join("stdout")).expect("opens the log");
    let err = std::fs::File::create(folder.join("stderr")).expect("opens the log");
    use std::os::unix::process::CommandExt;
    let mut child = {
        let mut run = lock(run);
        if run.aborted {
            return;
        }
        let child = Command::new(&words[0])
            .args(&words[1..])
            .current_dir(&folder)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .spawn()
            .expect("starts the container");
        run.group = Some(child.id());
        run.started = true;
        child
    };
    let status = child.wait().expect("the container ends");
    use std::os::unix::process::ExitStatusExt;
    let mut run = lock(run);
    run.group = None;
    run.exit_code = Some(status.code().unwrap_or(128 + status.signal().unwrap_or(9)));
}

fn details(run: &Run) -> Value {
    let phase = match (run.aborted, run.exit_code, run.started) {
        (true, _, _) => "ACTION_PHASE_ABORTED",
        (false, Some(0), _) => "ACTION_PHASE_SUCCEEDED",
        (false, Some(_), _) => "ACTION_PHASE_FAILED",
        (false, None, true) => "ACTION_PHASE_RUNNING",
        (false, None, false) => "ACTION_PHASE_QUEUED",
    };
    let mut action = json!({"id": {"name": "a0"}, "status": {"phase": phase, "attempts": 1}});
    if let Some(code) = run.exit_code.filter(|code| *code != 0) {
        action["errorInfo"] = json!({"message": format!("exit code {code}"), "kind": "KIND_USER"});
    }
    json!({"details": {"action": action}})
}

/// The log of a run as one message of a Connect stream, and the end of the stream.
fn send_log(stream: &mut TcpStream, folder: Option<PathBuf>) {
    let mut lines = Vec::new();
    for name in ["stdout", "stderr"] {
        let text =
            (folder.as_ref()).and_then(|folder| std::fs::read_to_string(folder.join(name)).ok());
        for line in text.unwrap_or_default().lines() {
            lines.push(json!({"message": line, "originator": "USER"}));
        }
    }
    let mut body = Vec::new();
    for (flags, message) in [(0u8, json!({"logs": [{"lines": lines}]})), (2, json!({}))] {
        let text = message.to_string();
        body.push(flags);
        body.extend((text.len() as u32).to_be_bytes());
        body.extend(text.as_bytes());
    }
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/connect+json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(&body);
}

fn send(stream: &mut TcpStream, status: u16, reply: &Value) {
    let text = reply.to_string();
    let reason = if status == 200 { "OK" } else { "Error" };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{text}",
        text.len()
    );
}

/// The path, the lower-case headers and the body of one request.
fn read(stream: &mut TcpStream) -> Option<(String, BTreeMap<String, String>, Vec<u8>)> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut first = String::new();
    reader.read_line(&mut first).ok()?;
    let mut words = first.split_whitespace();
    let (_, path) = (words.next()?, words.next()?.to_owned());
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
