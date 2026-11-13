//! A fake of the REST API of a wr manager on 127.0.0.1 for tests: HTTPS with a throwaway CA, a
//! bearer token, and jobs whose commands run here with `bash` in their folders.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
#![allow(dead_code, reason = "each test file uses only a part of this module")]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
use serde_json::{Value, json};
use tokio_rustls::rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::rustls::{ServerConfig, ServerConnection, StreamOwned, crypto};

/// The token that the fake takes.
pub const TOKEN: &str = "not-a-real-wr-token";

/// A fake manager, whose state the test reads while it runs.
pub struct FakeWr {
    /// The address of the API, like `https://127.0.0.1:4711`.
    pub address: String,
    /// The PEM text of the CA that signed the certificate of the fake.
    pub ca: String,
    state: Arc<Mutex<State>>,
}

/// One request: its method, its path with the query, its body, and whether it had the token.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub body: Value,
    pub authorized: bool,
}

#[derive(Default)]
struct State {
    requests: Vec<Request>,
    jobs: BTreeMap<String, Arc<Mutex<Job>>>,
    /// The folder that the jobs find first on their PATH.
    path: Option<PathBuf>,
}

#[derive(Default)]
struct Job {
    body: Value,
    state: String,
    exit_code: Option<i32>,
    /// The process group of the command while it runs.
    group: Option<u32>,
}

/// A new CA: its parameters and key, which sign certificates, and its PEM text.
pub fn authority() -> (Issuer<'static, KeyPair>, String) {
    let key = KeyPair::generate().expect("a key");
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("parameters");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let pem = params.self_signed(&key).expect("a CA").pem();
    (Issuer::new(params, key), pem)
}

impl FakeWr {
    /// Starts a manager whose jobs find `path` first on their PATH.
    pub fn start(path: Option<&Path>) -> FakeWr {
        let (issuer, ca) = authority();
        let key = KeyPair::generate().expect("a key");
        let names = vec!["localhost".to_owned(), "127.0.0.1".to_owned()];
        let params = CertificateParams::new(names).expect("parameters");
        let certificate = params.signed_by(&key, &issuer).expect("a certificate");
        let provider = Arc::new(crypto::aws_lc_rs::default_provider());
        let config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("TLS versions")
            .with_no_client_auth()
            .with_single_cert(
                vec![certificate.der().clone()],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
            )
            .expect("a server config");
        let config = Arc::new(config);
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        let address = format!("https://{}", listener.local_addr().expect("an address"));
        let state = Arc::new(Mutex::new(State {
            path: path.map(Path::to_owned),
            ..State::default()
        }));
        let shared = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (state, config) = (shared.clone(), config.clone());
                std::thread::spawn(move || {
                    let Ok(connection) = ServerConnection::new(config) else {
                        return;
                    };
                    let mut stream = StreamOwned::new(connection, stream);
                    answer(&state, &mut stream);
                });
            }
        });
        FakeWr { address, ca, state }
    }

    /// Writes the token and the CA where a wr manager of the deployment keeps them in `home`.
    pub fn write_files(&self, home: &Path, deployment: &str) {
        let folder = home.join(format!(".wr_{deployment}"));
        std::fs::create_dir_all(&folder).expect("makes the folder");
        std::fs::write(folder.join("client.token"), format!("{TOKEN}\n")).expect("writes");
        std::fs::write(folder.join("ca.pem"), &self.ca).expect("writes the CA");
    }

    /// The requests so far whose method and path start with `prefix`, like `POST /rest/v1/jobs/`.
    pub fn requests(&self, prefix: &str) -> Vec<Request> {
        let state = lock(&self.state);
        let requests = (state.requests.iter())
            .filter(|request| format!("{} {}", request.method, request.path).starts_with(prefix));
        requests.cloned().collect()
    }

    /// The state of the job `key`, like `running`; `None` for one that is not in the queue.
    pub fn state(&self, key: &str) -> Option<String> {
        let job = lock(&self.state).jobs.get(key).cloned();
        job.map(|job| lock(&job).state.clone())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn answer(state: &Arc<Mutex<State>>, stream: &mut StreamOwned<ServerConnection, TcpStream>) {
    let Some((method, path, headers, body)) = read(stream) else {
        return;
    };
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let authorized = headers.get("authorization") == Some(&format!("Bearer {TOKEN}"));
    let (route, query) = path.split_once('?').unwrap_or((&path, ""));
    let mut locked = lock(state);
    locked.requests.push(Request {
        method: method.clone(),
        path: path.clone(),
        body: body.clone(),
        authorized,
    });
    let keys = route.strip_prefix("/rest/v1/jobs/").unwrap_or_default();
    let (status, reply) = match (method.as_str(), keys) {
        _ if !authorized => (401, json!("unauthorized")),
        ("POST", "") => {
            let mut added = Vec::new();
            for job in body.as_array().into_iter().flatten() {
                let key = format!("{:032x}", locked.jobs.len() + 1);
                let entry = Arc::new(Mutex::new(Job {
                    body: job.clone(),
                    state: "ready".into(),
                    ..Job::default()
                }));
                locked.jobs.insert(key.clone(), entry.clone());
                let path = locked.path.clone();
                std::thread::spawn(move || run(&entry, path.as_deref()));
                added.push(json!({"Key": key, "State": "ready", "Cmd": job["cmd"]}));
            }
            (201, Value::Array(added))
        }
        ("GET", keys) => {
            let jobs = (keys.split(','))
                .filter_map(|key| locked.jobs.get(key).map(|job| (key, job)))
                .map(|(key, job)| status(key, &lock(job)))
                .collect();
            (200, Value::Array(jobs))
        }
        ("DELETE", key) => match (locked.jobs.get(key).cloned(), query) {
            (Some(job), "state=running") => {
                let mut job = lock(&job);
                if let Some(group) = job.group.take() {
                    let group = format!("-{group}");
                    let _ = Command::new("kill").args(["-KILL", "--", &group]).status();
                    job.state = "buried".into();
                }
                (200, json!([status(key, &job)]))
            }
            (Some(job), "state=deletable") if lock(&job).state != "running" => {
                locked.jobs.remove(key);
                (200, json!([]))
            }
            (_, _) => (200, json!([])),
        },
        _ => (404, json!("unknown call")),
    };
    drop(locked);
    let text = reply.to_string();
    let _ = write!(
        stream,
        "HTTP/1.1 {status} Answer\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{text}",
        text.len()
    );
    stream.conn.send_close_notify();
    let _ = stream.flush();
}

/// A job as `GET /rest/v1/jobs/` describes it.
fn status(key: &str, job: &Job) -> Value {
    let failed = job.state == "buried";
    json!({
        "Key": key, "State": job.state, "Cmd": job.body["cmd"], "Cwd": job.body["cwd"],
        "Exited": job.exit_code.is_some(), "Exitcode": job.exit_code.unwrap_or(0),
        "FailReason": if failed { "command exited non-zero" } else { "" },
    })
}

/// Runs the command of a job with `bash` in its folder, or in a new folder of its own when the
/// folder does not matter, as a wr runner does.
fn run(job: &Mutex<Job>, path: Option<&Path>) {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    let child = {
        let mut job = lock(job);
        let body = job.body.clone();
        let mut folder = PathBuf::from(body["cwd"].as_str().unwrap_or("/tmp"));
        if body["cwd_matters"] != true {
            folder = tempfile::Builder::new()
                .prefix("wr_cwd")
                .tempdir_in(&folder)
                .expect("a job folder")
                .keep();
        }
        let mut command = Command::new("bash");
        command
            .args(["-c", body["cmd"].as_str().unwrap_or("false")])
            .current_dir(&folder)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(path) = path {
            let paths = std::env::var("PATH").unwrap_or_default();
            command.env("PATH", format!("{}:{paths}", path.display()));
        }
        let child = command.spawn().expect("starts the command");
        job.group = Some(child.id());
        job.state = "running".into();
        (child, folder, body["on_exit"] == json!([{"cleanup": true}]))
    };
    let (mut child, folder, cleanup) = child;
    let status = child.wait().expect("the command ends");
    if cleanup {
        let _ = std::fs::remove_dir_all(&folder);
    }
    let mut job = lock(job);
    if job.group.take().is_none() {
        return;
    }
    let code = status.code().unwrap_or(128 + status.signal().unwrap_or(9));
    job.exit_code = Some(code);
    job.state = if code == 0 { "complete" } else { "buried" }.into();
}

/// The method, the path with its query, the lower-case headers and the body of one request.
#[expect(clippy::type_complexity, reason = "the parts of one HTTP request")]
fn read(stream: &mut impl Read) -> Option<(String, String, BTreeMap<String, String>, Vec<u8>)> {
    let mut reader = BufReader::new(stream);
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
    let length: usize = (headers.get("content-length"))
        .and_then(|text| text.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    Some((method, path, headers, body))
}
