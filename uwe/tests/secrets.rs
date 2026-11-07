//! Secret values that a host gives per run: concurrent runs keep them apart, and no record
//! of a run holds them.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde_json::json;
use uwe::RunRequest;
use uwe::run::{SecretValues, Settings};

/// The name both runs give a value for. The process environment never has it.
const NAME: &str = "UWE_TEST_TES_SECRET";

/// The values of the two runs. Asserts compare them without printing them.
const VALUES: [&str; 2] = ["first-run-value", "second-run-value"];

/// The child process that runs a test reads no site or user config file.
const CHILD: &str = "UWE_SECRETS_CHILD";

#[test]
fn keeps_secrets_apart() {
    if std::env::var_os(CHILD).is_none() {
        return in_child("keeps_secrets_apart");
    }
    let root = tempfile::tempdir().unwrap();
    // A token from the config file, then `UWE_TES_TOKEN` itself.
    let cases = [
        (format!("token = {{ environment = \"{NAME}\" }}\n"), NAME),
        (String::new(), "UWE_TES_TOKEN"),
    ];
    for (case, (token, name)) in cases.iter().enumerate() {
        let meeting = Arc::new((Mutex::new(0), Condvar::new()));
        let servers = [0, 1].map(|_| Server::start(meeting.clone()));
        let requests = [0, 1].map(|run| {
            let folder = root.path().join(format!("{case}-{run}"));
            let config = format!(
                "[executor.tes]\nserver = \"{}\"\n{token}",
                servers[run].address
            );
            let mut secrets = SecretValues::default();
            secrets.insert(*name, VALUES[run]).unwrap();
            secrets.allow_environment(false);
            assert!(
                !format!("{secrets:?}").contains(VALUES[run]),
                "Debug shows a value"
            );
            request(&folder, &config, secrets)
        });
        let [first, second] = requests;
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (first, second) = runtime.block_on(async { tokio::join!(first.run(), second.run()) });
        for (run, result) in [first, second].into_iter().enumerate() {
            let ran = result.unwrap_or_else(|failure| panic!("run {run}: {}", failure.message));
            assert!(ran.execution.error.is_none(), "{:?}", ran.execution.error);
            let seen = servers[run].tokens.lock().unwrap();
            assert!(!seen.is_empty(), "run {run} sent no request");
            let own = format!("Bearer {}", VALUES[run]);
            assert!(
                seen.iter().all(|token| *token == own),
                "run {run} sent another token"
            );
            let folder = root.path().join(format!("{case}-{run}"));
            assert_unrecorded(&folder, &ran.id);
            assert!(
                servers[run].met.load(Ordering::SeqCst),
                "the runs did not overlap"
            );
        }
    }
}

#[test]
fn refuses_missing_secret() {
    if std::env::var_os(CHILD).is_none() {
        return in_child("refuses_missing_secret");
    }
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(Arc::new((Mutex::new(0), Condvar::new())));
    let config = format!(
        "[executor.tes]\nserver = \"{}\"\ntoken = {{ environment = \"PATH\" }}\n",
        server.address
    );
    let mut secrets = SecretValues::default();
    secrets.allow_environment(false);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let Err(failure) = runtime.block_on(request(root.path(), &config, secrets).run()) else {
        panic!("the run started without a value for PATH");
    };
    assert!(
        failure
            .message
            .contains("the host gave no value for the secret PATH"),
        "{}",
        failure.message
    );
    assert!(server.tokens.lock().unwrap().is_empty());
}

/// Runs one test of this file again in a child process without config files, and checks that
/// its output holds no secret value.
fn in_child(test: &str) {
    let config = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .env("UWE_SITE_CONFIG", "")
        .env("XDG_CONFIG_HOME", config.path())
        .env_remove(NAME)
        .env_remove("UWE_TES_TOKEN")
        .output()
        .unwrap();
    let text = [output.stdout, output.stderr].concat();
    let text = String::from_utf8_lossy(&text);
    let leaked = VALUES.iter().any(|value| text.contains(value));
    assert!(!leaked, "the output of {test} holds a secret value");
    assert!(output.status.success(), "{test} failed:\n{text}");
}

/// A run without steps of a CWL workflow on the TES server of `config`.
fn request(folder: &Path, config: &str, secrets: SecretValues) -> RunRequest {
    std::fs::create_dir_all(folder).unwrap();
    let workflow = folder.join("flow.cwl");
    std::fs::write(
        &workflow,
        "cwlVersion: v1.2\nclass: Workflow\ninputs:\n  said: string\noutputs:\n  out:\n    type: string\n    outputSource: said\nsteps: []\n",
    )
    .unwrap();
    std::fs::write(folder.join("uwe.toml"), config).unwrap();
    let mut settings = Settings::new(&workflow);
    settings.executor = Some("tes".into());
    settings.config_file = Some(folder.join("uwe.toml"));
    settings.database = Some(folder.join("runs.sqlite"));
    settings.store = Some(folder.join("store").to_string_lossy().into_owned());
    settings.output_dir = Some(folder.join("outputs"));
    settings.cache = false;
    settings.cache_folder = Some(folder.join("cache"));
    settings.inputs = Some(json!({"said": "hello"}));
    settings.secrets = secrets;
    let mut request = RunRequest::new(settings);
    request.policy = uwe::application::Policy::Relaxed;
    request
}

/// Checks that the run database and the RO-Crate of the run hold no secret value.
fn assert_unrecorded(folder: &Path, run: &str) {
    let database = run_database::Database::open(&folder.join("runs.sqlite")).unwrap();
    let options = run_crate::Options {
        workflow: folder.join("flow.cwl"),
        ..run_crate::Options::default()
    };
    run_crate::export(&database, run, &folder.join("crate"), &options).unwrap();
    drop(database);
    let mut pending = vec![folder.to_owned()];
    let mut files = 0;
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let held = VALUES
                .iter()
                .any(|value| contains(&bytes, value.as_bytes()));
            assert!(!held, "{} holds a secret value", path.display());
            files += 1;
        }
    }
    assert!(folder.join("crate").join(run_crate::METADATA).is_file());
    assert!(files > 2, "only {files} files were checked");
}

fn contains(bytes: &[u8], part: &[u8]) -> bool {
    bytes.windows(part.len()).any(|window| window == part)
}

/// A TES server that answers every request with an empty list and keeps the Authorization
/// header of each. On its first request it waits until the other server has one too.
struct Server {
    address: String,
    tokens: Arc<Mutex<Vec<String>>>,
    /// Whether the other server had its first request while this one waited.
    met: Arc<AtomicBool>,
}

impl Server {
    fn start(meeting: Arc<(Mutex<usize>, Condvar)>) -> Server {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let tokens = Arc::new(Mutex::new(Vec::new()));
        let kept = tokens.clone();
        let met = Arc::new(AtomicBool::new(false));
        let other = met.clone();
        std::thread::spawn(move || {
            let mut first = true;
            for mut stream in listener.incoming().map_while(Result::ok) {
                let head = read_head(&mut stream);
                let token = head.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("authorization")
                        .then(|| value.trim().to_owned())
                });
                kept.lock().unwrap().push(token.unwrap_or_default());
                if std::mem::take(&mut first) {
                    other.store(meet(&meeting), Ordering::SeqCst);
                }
                let answer = r#"{"tasks":[]}"#;
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                );
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        Server {
            address,
            tokens,
            met,
        }
    }
}

/// Counts this server in and waits, for at most a minute, until both servers are counted.
/// Returns whether they were.
fn meet(meeting: &(Mutex<usize>, Condvar)) -> bool {
    let (count, changed) = meeting;
    let mut count = count.lock().unwrap();
    *count += 1;
    changed.notify_all();
    let limit = Duration::from_secs(60);
    let (_count, waited) = changed
        .wait_timeout_while(count, limit, |count| *count < 2)
        .unwrap();
    !waited.timed_out()
}

/// The request line and headers, up to the empty line.
fn read_head(stream: &mut std::net::TcpStream) -> String {
    let mut head = Vec::new();
    let mut buffer = [0; 4096];
    while !contains(&head, b"\r\n\r\n") {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => head.extend_from_slice(&buffer[..read]),
        }
    }
    String::from_utf8_lossy(&head).into_owned()
}
