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
use uwe::run::orphans::Outcome;
use uwe::run::{SecretValues, Settings};

/// The name both runs give a value for. The process environment never has it.
const NAME: &str = "UWE_TEST_TES_SECRET";

/// The values of the two runs. Asserts compare them without printing them.
const VALUES: [&str; 2] = ["first-run-value", "second-run-value"];

/// The child process that runs a test reads no site or user config file.
const CHILD: &str = "UWE_SECRETS_CHILD";

/// The variable that the Nextflow runs read, with `env` and as the key of nf-gpt.
const KEY: &str = "OPENAI_API_KEY";

/// The value of `KEY` in the environment of the child process.
const PROCESS_VALUE: &str = "process-value";

/// Writes what `env` gives for `KEY`, then asks nf-gpt, which sends `KEY` as its token.
const SCRIPT: &str = "include { gptPromptForText } from 'plugin/nf-gpt'\n\
    workflow {\n  file(params.seen).text = env('OPENAI_API_KEY') ?: 'none'\n  \
    file(params.answer).text = gptPromptForText('Tell me a joke')\n}\n";

/// A chat completion with one answer, as nf-gpt reads it.
const ANSWER: &str = r#"{"choices":[{"message":{"role":"assistant","content":"A joke"}}]}"#;

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

/// What the server says of every task: it has ended.
const ENDED: &str = r#"{"id":"task-1","state":"CANCELED"}"#;

#[test]
fn stops_with_secrets() {
    if std::env::var_os(CHILD).is_none() {
        return in_child("stops_with_secrets");
    }
    let root = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for (run, value) in VALUES.iter().enumerate() {
        let server = Server::answering(Arc::new((Mutex::new(1), Condvar::new())), ENDED);
        let folder = root.path().join(run.to_string());
        let config = format!(
            "[executor.tes]\nserver = \"{}\"\ntoken = {{ environment = \"{NAME}\" }}\n",
            server.address
        );
        let id = format!("run-{run}");
        let request = || {
            let mut secrets = SecretValues::default();
            secrets.insert(NAME, *value).unwrap();
            secrets.allow_environment(false);
            let mut request = request(&folder, &config, secrets);
            request.settings.run_id = Some(id.clone());
            request
        };
        // The journal of an attempt whose uwe process is gone.
        let attempt = folder.join(format!("cache/scratch/tes/{id}-sort-1-1"));
        std::fs::create_dir_all(&attempt).unwrap();
        std::fs::File::create(attempt.join("remote.lock")).unwrap();
        let journal = json!({"backend": "TES", "server": format!("{}/ga4gh/tes/v1", server.address),
            "name": format!("{id}-sort-1-1"), "run": id, "id": "task-1", "owner": u32::MAX,
            "host": "previous-boot", "started": "", "locked": true});
        std::fs::write(attempt.join("remote.json"), journal.to_string()).unwrap();
        let report = runtime.block_on(request().remote_tasks());
        let report = report.unwrap_or_else(|failure| panic!("run {run}: {}", failure.message));
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].state.as_deref(), Some("CANCELED"));
        assert_eq!(report[0].outcome, Outcome::Ended);
        assert!(
            attempt.join("remote.json").is_file(),
            "the report changed the journal"
        );
        let stopped = runtime.block_on(request().stop_tasks());
        let stopped = stopped.unwrap_or_else(|failure| panic!("run {run}: {}", failure.message));
        assert_eq!(stopped[0].outcome, Outcome::Ended);
        assert!(!attempt.join("remote.json").exists());
        let asked = {
            let tokens = server.tokens.lock().unwrap();
            let own = format!("Bearer {value}");
            assert!(!tokens.is_empty(), "run {run} sent no request");
            assert!(
                tokens.iter().all(|token| *token == own),
                "run {run} sent another token"
            );
            tokens.len()
        };
        // Once the journal is settled, a second call asks no server.
        let again = runtime.block_on(request().stop_tasks());
        assert!(
            again
                .unwrap_or_else(|failure| panic!("{}", failure.message))
                .is_empty()
        );
        assert_eq!(server.tokens.lock().unwrap().len(), asked);
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

#[test]
fn routes_workflow_environment() {
    if std::env::var_os(CHILD).is_none() {
        return in_child("routes_workflow_environment");
    }
    let root = tempfile::tempdir().unwrap();
    let meeting = Arc::new((Mutex::new(0), Condvar::new()));
    let servers = [0, 1].map(|_| Server::answering(meeting.clone(), ANSWER));
    let requests = [0, 1].map(|run| {
        let mut secrets = SecretValues::default();
        secrets.insert(KEY, VALUES[run]).unwrap();
        secrets.allow_environment(false);
        nextflow(&root.path().join(run.to_string()), &servers[run], secrets)
    });
    let [first, second] = requests;
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (first, second) = runtime.block_on(async { tokio::join!(first.run(), second.run()) });
    for (run, result) in [first, second].into_iter().enumerate() {
        let ran = result.unwrap_or_else(|failure| panic!("run {run}: {}", failure.message));
        assert!(ran.execution.error.is_none(), "{:?}", ran.execution.error);
        let seen = std::fs::read_to_string(root.path().join(format!("{run}/seen.txt"))).unwrap();
        assert!(seen == VALUES[run], "env gave run {run} another value");
        let own = format!("Bearer {}", VALUES[run]);
        let tokens = servers[run].tokens.lock().unwrap();
        assert!(!tokens.is_empty(), "run {run} asked nf-gpt nothing");
        assert!(
            tokens.iter().all(|token| *token == own),
            "run {run} sent another key"
        );
        assert!(
            servers[run].met.load(Ordering::SeqCst),
            "the runs did not overlap"
        );
    }
    // Without a given value and without the environment, the key is not set.
    let server = Server::answering(Arc::new((Mutex::new(1), Condvar::new())), ANSWER);
    let mut secrets = SecretValues::default();
    secrets.allow_environment(false);
    let folder = root.path().join("unset");
    let result = runtime.block_on(nextflow(&folder, &server, secrets).run());
    let message = match result {
        Ok(ran) => ran
            .execution
            .error
            .map(|error| error.to_string())
            .unwrap_or_default(),
        Err(failure) => failure.message,
    };
    assert!(message.contains("Missing ChatGPT API key"), "{message}");
    let seen = std::fs::read_to_string(folder.join("seen.txt")).unwrap();
    assert_eq!(seen, "none");
    assert!(server.tokens.lock().unwrap().is_empty());
    // The command line reads the environment of the process.
    let folder = root.path().join("process");
    let result = runtime.block_on(nextflow(&folder, &server, SecretValues::default()).run());
    let ran = result.unwrap_or_else(|failure| panic!("{}", failure.message));
    assert!(ran.execution.error.is_none(), "{:?}", ran.execution.error);
    let seen = std::fs::read_to_string(folder.join("seen.txt")).unwrap();
    assert_eq!(seen, PROCESS_VALUE);
    let tokens = server.tokens.lock().unwrap();
    assert_eq!(*tokens, [format!("Bearer {PROCESS_VALUE}")]);
}

/// Asks for `KEY` with `envvars` and compares the length of its value in `os.environ` with the
/// config, so that no file holds the value.
const SNAKEFILE: &str = "import os\nconfigfile: \"config.yaml\"\nenvvars: \"OPENAI_API_KEY\"\n\
    if len(os.environ[\"OPENAI_API_KEY\"]) != config[\"length\"]:\n    raise ValueError(\"another value\")\n\
    rule all:\n    output: \"done.txt\"\n    shell: \"touch done.txt\"\n";

#[test]
fn routes_snakemake_environment() {
    if std::env::var_os(CHILD).is_none() {
        return in_child("routes_snakemake_environment");
    }
    let root = tempfile::tempdir().unwrap();
    let requests = [0, 1].map(|run| {
        let mut secrets = SecretValues::default();
        secrets.insert(KEY, VALUES[run]).unwrap();
        secrets.allow_environment(false);
        let folder = root.path().join(run.to_string());
        snakemake(&folder, VALUES[run].len(), secrets)
    });
    let [first, second] = requests;
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (first, second) = runtime.block_on(async { tokio::join!(first.run(), second.run()) });
    for (run, result) in [first, second].into_iter().enumerate() {
        let ran = result.unwrap_or_else(|failure| panic!("run {run}: {}", failure.message));
        assert!(
            ran.execution.error.is_none(),
            "run {run}: {:?}",
            ran.execution.error
        );
    }
    // Without a given value and without the environment, `envvars` finds the name unset.
    let mut secrets = SecretValues::default();
    secrets.allow_environment(false);
    let unset = snakemake(&root.path().join("unset"), PROCESS_VALUE.len(), secrets);
    let message = match runtime.block_on(unset.run()) {
        Ok(ran) => (ran.execution.error)
            .map(|error| error.to_string())
            .unwrap_or_default(),
        Err(failure) => failure.message,
    };
    assert!(
        message.contains("requested by the workflow but undefined"),
        "{message}"
    );
    // The command line reads the environment of the process.
    let folder = root.path().join("process");
    let process = snakemake(&folder, PROCESS_VALUE.len(), SecretValues::default());
    let ran = runtime.block_on(process.run());
    let ran = ran.unwrap_or_else(|failure| panic!("{}", failure.message));
    assert!(ran.execution.error.is_none(), "{:?}", ran.execution.error);
}

/// A simulated run of the project `folder` with `SNAKEFILE`, whose config asks for a value of
/// `length`.
fn snakemake(folder: &Path, length: usize, secrets: SecretValues) -> RunRequest {
    std::fs::create_dir_all(folder).unwrap();
    std::fs::write(folder.join("Snakefile"), SNAKEFILE).unwrap();
    std::fs::write(folder.join("config.yaml"), format!("length: {length}\n")).unwrap();
    let mut settings = Settings::new(folder);
    settings.executor = Some("simulator".into());
    settings.database = Some(folder.join("runs.sqlite"));
    settings.store = Some(folder.join("store").to_string_lossy().into_owned());
    settings.output_dir = Some(folder.join("outputs"));
    settings.cache = false;
    settings.cache_folder = Some(folder.join("cache"));
    settings.secrets = secrets;
    let mut request = RunRequest::new(settings);
    request.policy = uwe::application::Policy::Relaxed;
    request
}

/// A run of `SCRIPT` in `folder`, with nf-gpt asking `server`.
fn nextflow(folder: &Path, server: &Server, secrets: SecretValues) -> RunRequest {
    std::fs::create_dir_all(folder).unwrap();
    std::fs::write(folder.join("main.nf"), SCRIPT).unwrap();
    let config = format!(
        "plugins {{ id 'nf-gpt@0.4.0' }}\ngpt {{ endpoint = '{}' }}\n",
        server.address
    );
    std::fs::write(folder.join("nextflow.config"), config).unwrap();
    let mut settings = Settings::new(folder.join("main.nf"));
    let path = |name: &str| json!(folder.join(name).to_string_lossy());
    settings.params.insert("seen".into(), path("seen.txt"));
    settings.params.insert("answer".into(), path("answer.txt"));
    settings.database = Some(folder.join("runs.sqlite"));
    settings.store = Some(folder.join("store").to_string_lossy().into_owned());
    settings.output_dir = Some(folder.join("outputs"));
    settings.cache = false;
    settings.cache_folder = Some(folder.join("cache"));
    settings.secrets = secrets;
    let mut request = RunRequest::new(settings);
    request.policy = uwe::application::Policy::Relaxed;
    request
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
        .env(KEY, PROCESS_VALUE)
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
        Server::answering(meeting, r#"{"tasks":[]}"#)
    }

    /// A server that answers every request with `answer`.
    fn answering(meeting: Arc<(Mutex<usize>, Condvar)>, answer: &'static str) -> Server {
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

/// The request line and headers, up to the empty line. Reads the body too, so that the
/// answer does not reset the connection.
fn read_head(stream: &mut std::net::TcpStream) -> String {
    let mut head = Vec::new();
    let mut buffer = [0; 4096];
    while !contains(&head, b"\r\n\r\n") {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => head.extend_from_slice(&buffer[..read]),
        }
    }
    let text = String::from_utf8_lossy(&head).into_owned();
    let length: usize = (text.lines())
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())?
        })
        .unwrap_or(0);
    let start = (head.windows(4)).position(|window| window == b"\r\n\r\n");
    let mut read = head.len() - start.map_or(head.len(), |start| start + 4);
    while read < length {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(more) => read += more,
        }
    }
    text
}
