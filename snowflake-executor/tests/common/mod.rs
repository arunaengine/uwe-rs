//! A fake of the Snowflake SQL API on 127.0.0.1 for tests: it runs the containers of each job
//! service here, and answers the statements that read their logs and drop them.
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
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};

/// The subject of the key-pair tokens and the access token that the fake accepts.
pub const SUBJECT: &str = "MYORG-LAB.UWE";
pub const TOKEN: &str = "not-a-real-token";

/// A fake server, whose state the test reads while it runs.
pub struct FakeSnowflake {
    /// The address of the server, like `http://127.0.0.1:4711`.
    pub address: String,
    state: Arc<Mutex<State>>,
}

/// One request: its method, path with query, token type header and body.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub kind: String,
    pub body: Vec<u8>,
}

impl Request {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The statement of a submission.
    pub fn statement(&self) -> String {
        self.json()["statement"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }
}

#[derive(Default)]
struct State {
    folder: PathBuf,
    /// The program that stands for `/uwe` of a helper image.
    program: Option<PathBuf>,
    requests: Vec<Request>,
    statements: BTreeMap<String, Arc<Mutex<Statement>>>,
    /// The handles by request id, as a retried submission gets its first statement.
    requested: BTreeMap<String, String>,
}

#[derive(Default)]
struct Statement {
    /// The job service that it runs, by name.
    job: Option<String>,
    /// The process group of the main container while it runs.
    group: Option<u32>,
    done: Option<Result<Value, String>>,
    folder: PathBuf,
}

impl FakeSnowflake {
    /// Starts a server that keeps the folders of jobs in `folder`; `program` stands for the
    /// `/uwe` of helper images.
    pub fn start(folder: &Path, program: Option<PathBuf>) -> FakeSnowflake {
        std::fs::create_dir_all(folder).expect("makes the folder of the fake");
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        let address = format!("http://{}", listener.local_addr().expect("an address"));
        let state = Arc::new(Mutex::new(State {
            folder: folder.to_owned(),
            program,
            ..State::default()
        }));
        let shared = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let state = shared.clone();
                std::thread::spawn(move || answer(&state, stream));
            }
        });
        FakeSnowflake { address, state }
    }

    /// The requests so far whose method and path start with `prefix`.
    pub fn requests(&self, prefix: &str) -> Vec<Request> {
        let state = lock(&self.state);
        let requests = (state.requests.iter())
            .filter(|request| format!("{} {}", request.method, request.path).starts_with(prefix));
        requests.cloned().collect()
    }

    /// The statements so far that start with `prefix`.
    pub fn statements(&self, prefix: &str) -> Vec<String> {
        (self.requests("POST /api/v2/statements?").iter())
            .map(Request::statement)
            .filter(|statement| statement.starts_with(prefix))
            .collect()
    }

    /// Whether the main container of a job service runs.
    pub fn running(&self) -> bool {
        let state = lock(&self.state);
        (state.statements.values()).any(|statement| lock(statement).group.is_some())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Whether a request carries a token that the fake takes.
fn signed(headers: &BTreeMap<String, String>) -> bool {
    let token = (headers.get("authorization")).and_then(|value| value.strip_prefix("Bearer "));
    let kind = headers
        .get("x-snowflake-authorization-token-type")
        .map(String::as_str);
    match (kind, token) {
        (Some("PROGRAMMATIC_ACCESS_TOKEN"), Some(token)) => token == TOKEN,
        (Some("KEYPAIR_JWT"), Some(token)) => {
            let payload = token.split('.').nth(1).unwrap_or_default();
            let claims: Value = (URL_SAFE_NO_PAD.decode(payload).ok())
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
            let issuer = claims["iss"].as_str().unwrap_or_default();
            claims["sub"] == SUBJECT && issuer.starts_with(&format!("{SUBJECT}.SHA256:"))
        }
        _ => false,
    }
}

fn answer(state: &Mutex<State>, mut stream: TcpStream) {
    let Some((method, path, headers, body)) = read(&mut stream) else {
        return;
    };
    let kind = (headers.get("x-snowflake-authorization-token-type").cloned()).unwrap_or_default();
    let mut state = lock(state);
    state.requests.push(Request {
        method: method.clone(),
        path: path.clone(),
        kind,
        body: body.clone(),
    });
    if !signed(&headers) {
        drop(state);
        let reply = json!({"code": "390144", "message": "JWT token is invalid."});
        return send(&mut stream, 401, &reply);
    }
    let route = path.split('?').next().unwrap_or_default().to_owned();
    let parts: Vec<&str> = route.trim_start_matches('/').split('/').collect();
    let (status, reply) = match (method.as_str(), parts.as_slice()) {
        ("POST", ["api", "v2", "statements"]) => {
            let request = (path.split("requestId=").nth(1))
                .map(|rest| rest.split('&').next().unwrap_or_default().to_owned())
                .unwrap_or_default();
            let handle = match state.requested.get(&request) {
                Some(handle) => handle.clone(),
                None => {
                    let handle = format!("01b0-{}", state.statements.len() + 1);
                    let statement = submit(&mut state, &body);
                    state.statements.insert(handle.clone(), statement);
                    state.requested.insert(request, handle.clone());
                    handle
                }
            };
            let message = "Asynchronous execution in progress. Use provided query id to perform \
                           query monitoring and management.";
            (
                202,
                json!({"code": "333334", "message": message, "statementHandle": handle}),
            )
        }
        ("GET", ["api", "v2", "statements", handle]) => match state.statements.get(*handle) {
            Some(statement) => status(handle, &lock(statement)),
            None => (404, json!({"message": "no such statement"})),
        },
        ("POST", ["api", "v2", "statements", handle, "cancel"]) => {
            match state.statements.get(*handle) {
                Some(statement) => {
                    let mut statement = lock(statement);
                    if let Some(group) = statement.group.take() {
                        let group = format!("-{group}");
                        let _ = Command::new("kill").args(["-KILL", "--", &group]).status();
                    }
                    statement.done = Some(Err("SQL execution canceled".into()));
                    (
                        200,
                        json!({"statementHandle": handle, "message": "successfully canceled"}),
                    )
                }
                None => (404, json!({"message": "no such statement"})),
            }
        }
        _ => (404, json!({"message": "unknown call"})),
    };
    drop(state);
    send(&mut stream, status, &reply);
}

/// A statement for a submission: a job service whose containers start now, or the answer to a
/// statement that reads a log or drops a job service.
fn submit(state: &mut State, body: &[u8]) -> Arc<Mutex<Statement>> {
    let body: Value = serde_json::from_slice(body).unwrap_or_default();
    let text = body["statement"].as_str().unwrap_or_default();
    let statement = Arc::new(Mutex::new(Statement::default()));
    if text.starts_with("EXECUTE JOB SERVICE") {
        let name = (text.lines())
            .find_map(|line| line.strip_prefix("NAME = "))
            .unwrap_or_default()
            .to_owned();
        let spec = (text.split("$$\n").nth(1))
            .and_then(|rest| rest.split("\n$$").next())
            .and_then(|spec| serde_json::from_str::<Value>(spec).ok())
            .unwrap_or_default();
        let folder = state.folder.join(&name);
        {
            let mut job = lock(&statement);
            job.job = Some(name);
            job.folder = folder;
        }
        let (job, program) = (statement.clone(), state.program.clone());
        std::thread::spawn(move || run(&job, &spec, program));
        return statement;
    }
    let done = if let Some(rest) = text.strip_prefix("SELECT SYSTEM$GET_SERVICE_LOGS('") {
        let name = rest.split('\'').next().unwrap_or_default();
        let job = (state.statements.values())
            .find(|statement| lock(statement).job.as_deref() == Some(name));
        let folder = job.map(|job| lock(job).folder.clone());
        let mut log = String::new();
        for file in ["stdout", "stderr"] {
            let path = folder.as_ref().map(|folder| folder.join(file));
            log.push_str(
                &path
                    .and_then(|path| std::fs::read_to_string(path).ok())
                    .unwrap_or_default(),
            );
        }
        Ok(json!([[log]]))
    } else if let Some(name) = text.strip_prefix("DROP SERVICE IF EXISTS ") {
        Ok(json!([[format!("{name} successfully dropped.")]]))
    } else {
        Err("SQL compilation error: unknown statement".to_owned())
    };
    lock(&statement).done = Some(done);
    statement
}

/// Runs the containers of a job service here, each in its own thread; the statement ends with
/// the main container.
fn run(statement: &Mutex<Statement>, spec: &Value, program: Option<PathBuf>) {
    let folder = lock(statement).folder.clone();
    std::fs::create_dir_all(&folder).expect("makes the folder of the job");
    let containers = spec["spec"]["containers"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for container in &containers {
        for mount in container["volumeMounts"].as_array().into_iter().flatten() {
            let path = mount["mountPath"].as_str().unwrap_or("/none");
            std::fs::create_dir_all(path).expect("makes the local volume");
        }
    }
    let mut helpers = Vec::new();
    for container in containers
        .iter()
        .filter(|container| container["name"] != "main")
    {
        let mut words = words(container);
        if words.first().map(String::as_str) == Some("/uwe") {
            let program = program.clone().expect("a program for the helper image");
            words[0] = program.to_string_lossy().into_owned();
        }
        let folder = folder.clone();
        helpers.push(std::thread::spawn(move || {
            let _ = Command::new(&words[0])
                .args(&words[1..])
                .current_dir(folder)
                .status();
        }));
    }
    let Some(main) = containers
        .iter()
        .find(|container| container["name"] == "main")
    else {
        lock(statement).done = Some(Err("the specification has no main container".into()));
        return;
    };
    let words = words(main);
    let out = std::fs::File::create(folder.join("stdout")).expect("opens the log");
    let err = std::fs::File::create(folder.join("stderr")).expect("opens the log");
    use std::os::unix::process::CommandExt;
    let mut child = {
        let mut statement = lock(statement);
        if statement.done.is_some() {
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
            .expect("starts the main container");
        statement.group = Some(child.id());
        child
    };
    let status = child.wait().expect("the container ends");
    for helper in helpers {
        let _ = helper.join();
    }
    let mut statement = lock(statement);
    if statement.group.take().is_none() {
        return;
    }
    let name = statement.job.clone().unwrap_or_default();
    statement.done = Some(match status.code() {
        Some(0) => Ok(json!([[format!(
            "Job {name} completed successfully with status: DONE."
        )]])),
        code => Err(format!(
            "Job {name} failed: container main exited with code {code:?}"
        )),
    });
}

/// The command and arguments of a container, without the entrypoint of its image.
fn words(container: &Value) -> Vec<String> {
    let texts = |key: &str| -> Vec<String> {
        (container[key].as_array().into_iter().flatten())
            .filter_map(|word| word.as_str().map(str::to_owned))
            .collect()
    };
    let mut words = texts("command");
    words.extend(texts("args"));
    words
}

fn status(handle: &str, statement: &Statement) -> (u16, Value) {
    match &statement.done {
        None => (
            202,
            json!({"code": "333334", "statementHandle": handle,
                             "message": "Asynchronous execution in progress."}),
        ),
        Some(Ok(data)) => (
            200,
            json!({"statementHandle": handle, "data": data,
                                       "resultSetMetaData": {"numRows": 1}}),
        ),
        Some(Err(message)) => (
            422,
            json!({"code": "000603", "statementHandle": handle,
                                           "message": message, "sqlState": "XX000"}),
        ),
    }
}

fn send(stream: &mut TcpStream, status: u16, reply: &Value) {
    let text = reply.to_string();
    let reason = if status == 200 { "OK" } else { "Other" };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{text}",
        text.len()
    );
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
