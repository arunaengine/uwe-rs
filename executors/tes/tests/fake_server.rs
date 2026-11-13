//! The TES backend against a small fake server on this machine, which answers as Funnel does
//! and writes the files that the task would give back.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use executor::registry::{ImageConfig, ImageConfigs};
use executor::{Backend, ExecutorError, Job, Staged, Staging};
use task_model::{Accelerator, ContainerSpec, FORMAT, Image, OutputRule, Placement, TaskSpec};
use tes_executor::{Client, TesBackend};
use tokio_util::sync::CancellationToken;

/// Reads one request: its first line and its body.
fn request(stream: &mut std::net::TcpStream) -> (String, String) {
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    reader.read_line(&mut first).expect("a request line");
    let mut length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("a header");
        if line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).expect("a body");
    (first, String::from_utf8_lossy(&body).into_owned())
}

/// Writes what a container would leave for an output of the task: the control files, or
/// `out.txt` in the outbox.
fn leave(output: &serde_json::Value) {
    let url = output["url"].as_str().expect("a URL");
    let folder = PathBuf::from(url.strip_prefix("file://").expect("a file URL"));
    std::fs::create_dir_all(&folder).expect("makes the folder");
    match output["path"].as_str() {
        Some("/uwe/control") => {
            std::fs::write(folder.join("exit"), "0\n").expect("writes");
            std::fs::write(folder.join("manifest"), "file out.txt\n").expect("writes");
            // The other container of a task of two.
            let other = folder.join("containers/prepare");
            std::fs::create_dir_all(&other).expect("makes the folder");
            std::fs::write(other.join("exit"), "0\n").expect("writes");
        }
        _ => std::fs::write(folder.join("out.txt"), "hi\n").expect("writes"),
    }
}

/// Makes a journal look an hour old, as recovery takes only a submission that old as lost.
fn age(journal: &std::path::Path) {
    let file = std::fs::File::options()
        .write(true)
        .open(journal)
        .expect("the journal");
    let time = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    file.set_modified(time).expect("sets the time");
}

/// A server that takes one task, gives back `out.txt` and exit code 0 when asked about it,
/// and keeps the task it got.
fn serve(task: Arc<Mutex<Option<serde_json::Value>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let address = format!("http://{}", listener.local_addr().expect("an address"));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let (first, body) = request(&mut stream);
            let answer = if first.starts_with("POST /ga4gh/tes/v1/tasks ") {
                let document: serde_json::Value = serde_json::from_str(&body).expect("JSON");
                for output in document["outputs"].as_array().expect("outputs") {
                    leave(output);
                }
                *task.lock().expect("the task") = Some(document);
                r#"{"id":"task-1"}"#.to_owned()
            } else {
                r#"{"id":"task-1","state":"COMPLETE"}"#.to_owned()
            };
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{answer}",
                answer.len()
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    address
}

fn simple_spec() -> TaskSpec {
    TaskSpec {
        run_as: None,
        format: FORMAT,
        namespace: String::new(),
        image: Some(Image {
            reference: "test".into(),
            platform: None,
        }),
        arguments: vec!["true".into()],
        workdir: None,
        environment: Default::default(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        output_manifest: None,
        stdin: None,
        containers: Vec::new(),
        replaces_entrypoint: true,
    }
}

fn process_start() -> String {
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", std::process::id())).unwrap();
    let (_, fields) = stat.rsplit_once(')').unwrap();
    fields.split_whitespace().nth(19).unwrap().to_owned()
}

#[test]
fn returns_server_outputs() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let task = Arc::new(Mutex::new(None));
    let address = serve(task.clone());
    // The image runs commands through an entrypoint, as Biocontainers do.
    let config = ImageConfig {
        entrypoint: vec!["/usr/local/env-execute".into()],
        cmd: Vec::new(),
        user: String::new(),
    };
    let backend = TesBackend {
        run: String::new(),
        client: Client::new(&address, None).expect("a client"),
        scratch: folder.path().join("scratch"),
        staging: None,
        settings: Default::default(),
        configs: ImageConfigs::known([("debian:stable-slim".to_owned(), config)]),
    };
    let source = folder.path().join("in.txt");
    std::fs::write(&source, "in\n").expect("writes the input");
    let spec = TaskSpec {
        run_as: None,
        format: FORMAT,
        namespace: String::new(),
        image: Some(Image {
            reference: "debian:stable-slim".into(),
            platform: None,
        }),
        arguments: vec!["bash".into(), "-c".into(), "cp in.txt out.txt".into()],
        workdir: None,
        environment: Default::default(),
        inputs: Vec::new(),
        outputs: vec![OutputRule {
            pattern: "out.txt".into(),
        }],
        output_manifest: None,
        stdin: None,
        containers: Vec::new(),
        replaces_entrypoint: false,
    };
    let staging = Staging {
        inputs: vec![Staged {
            path: "in.txt".into(),
            source,
            directory: false,
            copy: false,
            executable: false,
            ..Default::default()
        }],
        ..Staging::default()
    };
    let job = Job {
        name: "run-P-1-1",
        spec: &spec,
        staging: &staging,
        placement: &Placement::default(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let finished = runtime
        .block_on(backend.run(job, &CancellationToken::new()))
        .expect("runs");
    assert_eq!(finished.exit_code, Some(0));
    assert_eq!(finished.files.len(), 1);
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("reads");
    assert_eq!(output, "hi\n");
    let sent = task.lock().expect("the task").clone().expect("a task");
    assert_eq!(sent["executors"][0]["command"][1], "/uwe/wrapper.sh");
    let wrapper = std::fs::read_to_string(finished.root.join("wrapper.sh")).expect("reads");
    assert!(
        wrapper.contains("'/usr/local/env-execute' 'bash' '-c' 'cp in.txt out.txt'"),
        "{wrapper}"
    );
    assert_eq!(sent["inputs"][1]["path"], "/uwe/work/in.txt");
    let placement = Placement {
        offline: true,
        ..Placement::default()
    };
    let job = Job {
        name: "offline",
        placement: &placement,
        ..job
    };
    let result = runtime.block_on(backend.run(job, &CancellationToken::new()));
    assert!(
        matches!(result, Err(ExecutorError::Unsupported(_))),
        "{result:?}"
    );
    assert!(!backend.scratch.join("offline").exists());
    let placement = Placement {
        accelerators: vec![Accelerator {
            count: 1,
            kind: None,
        }],
        ..Placement::default()
    };
    let job = Job {
        name: "gpu",
        placement: &placement,
        ..job
    };
    assert!(matches!(
        runtime.block_on(backend.run(job, &CancellationToken::new())),
        Err(ExecutorError::Unsupported(_))
    ));
    assert!(!backend.scratch.join("gpu").exists());
}

#[test]
fn refuses_multiple_executors() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let task = Arc::new(Mutex::new(None));
    let address = serve(task.clone());
    let config = |entrypoint: &[&str]| ImageConfig {
        entrypoint: entrypoint.iter().map(|word| (*word).to_owned()).collect(),
        cmd: Vec::new(),
        user: String::new(),
    };
    let backend = TesBackend {
        run: String::new(),
        client: Client::new(&address, None).expect("a client"),
        scratch: folder.path().join("scratch"),
        staging: None,
        settings: Default::default(),
        configs: ImageConfigs::known([
            ("debian:stable-slim".to_owned(), config(&[])),
            ("alpine:latest".to_owned(), config(&["/entry"])),
        ]),
    };
    let image = |reference: &str| {
        Some(Image {
            reference: reference.into(),
            platform: None,
        })
    };
    let container = |name: &str, reference: &str, after: &[&str]| ContainerSpec {
        service: false,
        replaces_entrypoint: false,
        name: name.into(),
        image: image(reference),
        arguments: vec!["sh".into(), "-c".into(), format!("echo {name}")],
        workdir: None,
        environment: Default::default(),
        after: after.iter().map(|name| (*name).to_owned()).collect(),
        attempts: 1,
    };
    let spec = TaskSpec {
        run_as: None,
        format: FORMAT,
        namespace: String::new(),
        image: image("debian:stable-slim"),
        arguments: vec!["sh".into(), "-c".into(), "echo main".into()],
        workdir: None,
        environment: Default::default(),
        inputs: Vec::new(),
        outputs: vec![OutputRule {
            pattern: "out.txt".into(),
        }],
        output_manifest: None,
        stdin: None,
        containers: vec![
            container("main", "debian:stable-slim", &["prepare"]),
            container("prepare", "alpine:latest", &[]),
        ],
        replaces_entrypoint: false,
    };
    let job = Job {
        name: "run-S-1-1",
        spec: &spec,
        staging: &Staging::default(),
        placement: &Placement::default(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let error = runtime
        .block_on(backend.run(job, &CancellationToken::new()))
        .expect_err("one TES executor is required");
    assert!(matches!(error, ExecutorError::Unsupported(_)));
    assert!(task.lock().unwrap().is_none());
    assert!(!backend.scratch.join(job.name).exists());
}

#[test]
fn bounds_accepted_tasks() {
    // A server may report UNKNOWN instead of RUNNING; the time of the task runs out all the same.
    for (timed_out, state) in [(false, "RUNNING"), (true, "RUNNING"), (true, "UNKNOWN")] {
        let folder = tempfile::tempdir().expect("a folder");
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let address = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let started = format!(r#"{{"id":"task-1","state":"{state}"}}"#);
        let server = std::thread::spawn(move || {
            let answers = if timed_out {
                vec![
                    (200, r#"{"id":"task-1"}"#),
                    (200, started.as_str()),
                    (200, "{}"),
                    (200, r#"{"id":"task-1","state":"CANCELED"}"#),
                ]
            } else {
                vec![
                    (200, r#"{"id":"task-1"}"#),
                    (503, "unavailable"),
                    (200, r#"{"id":"task-1","state":"RUNNING"}"#),
                    (200, r#"{"id":"task-1","state":"COMPLETE"}"#),
                    (200, r#"{"id":"task-1","state":"COMPLETE"}"#),
                ]
            };
            for (status, answer) in answers {
                let (mut stream, _) = listener.accept().unwrap();
                let (first, body) = request(&mut stream);
                if first.starts_with("POST /ga4gh/tes/v1/tasks ") {
                    let document: serde_json::Value = serde_json::from_str(&body).unwrap();
                    for output in document["outputs"].as_array().unwrap() {
                        leave(output);
                    }
                }
                recorded.lock().unwrap().push(first);
                let reply = format!(
                    "HTTP/1.1 {status} Reply\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                );
                stream.write_all(reply.as_bytes()).unwrap();
            }
        });
        // No margin for staging, so that the deadline comes at once.
        let settings = tes_executor::Settings {
            staging_minutes: Some(0),
            ..Default::default()
        };
        let backend = TesBackend {
            run: String::new(),
            client: Client::new(&address, None).unwrap(),
            scratch: folder.path().join("scratch"),
            staging: None,
            settings,
            configs: ImageConfigs::known([("test".into(), ImageConfig::default())]),
        };
        let spec = TaskSpec {
            format: FORMAT,
            namespace: String::new(),
            image: Some(Image {
                reference: "test".into(),
                platform: None,
            }),
            arguments: vec!["true".into()],
            replaces_entrypoint: true,
            run_as: None,
            workdir: None,
            environment: Default::default(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            output_manifest: None,
            stdin: None,
            containers: Vec::new(),
        };
        let placement = Placement {
            time_seconds: timed_out.then_some(0),
            ..Placement::default()
        };
        let staging = Staging::default();
        let job = Job {
            name: "run-P-1-1",
            spec: &spec,
            staging: &staging,
            placement: &placement,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let finished = runtime
            .block_on(async {
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    backend.run(job, &CancellationToken::new()),
                )
                .await
            })
            .expect("bounded lifecycle")
            .expect("terminal task");
        server.join().unwrap();
        assert_eq!(finished.exit_code, Some(if timed_out { 143 } else { 0 }));
        assert!(!finished.root.join("remote.json").exists());
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls
                .iter()
                .filter(|line| line.starts_with("POST /ga4gh/tes/v1/tasks "))
                .count(),
            1
        );
        assert_eq!(calls.iter().any(|line| line.contains(":cancel")), timed_out);
        assert!(
            calls
                .iter()
                .filter(|line| line.starts_with("GET "))
                .all(|line| { line.starts_with("GET /ga4gh/tes/v1/tasks/task-1?view=") })
        );
        assert_eq!(
            calls
                .iter()
                .filter(|line| line.contains("?view=FULL"))
                .count(),
            1
        );
    }
}

#[test]
fn stops_task_time() {
    stop_task(false);
}

#[test]
fn keeps_own_timeouts() {
    stop_task(true);
}

/// A task that ends with 124: its timer ended it, or with `own` the task ended so itself, like
/// a `timeout` inside it.
fn stop_task(own: bool) {
    let folder = tempfile::tempdir().expect("a folder");
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let address = format!("http://{}", listener.local_addr().unwrap());
    let sent = Arc::new(Mutex::new(None));
    let kept = sent.clone();
    // The wrapper of the task ran out of its time, and its timer ended it with 124.
    let ended = r#"{"id":"task-1","state":"EXECUTOR_ERROR","logs":[{"logs":[{"exit_code":124}]}]}"#;
    let server = std::thread::spawn(move || {
        for (expected, answer) in [
            ("POST /ga4gh/tes/v1/tasks ", r#"{"id":"task-1"}"#),
            (
                "GET /ga4gh/tes/v1/tasks/task-1?view=MINIMAL ",
                r#"{"id":"task-1","state":"EXECUTOR_ERROR"}"#,
            ),
            ("GET /ga4gh/tes/v1/tasks/task-1?view=FULL ", ended),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let (first, body) = request(&mut stream);
            assert!(first.starts_with(expected), "{first}");
            if first.starts_with("POST /ga4gh/tes/v1/tasks ") {
                let document: serde_json::Value = serde_json::from_str(&body).unwrap();
                for output in document["outputs"].as_array().unwrap() {
                    leave(output);
                    let url = output["url"].as_str().unwrap();
                    if own && output["path"] == "/uwe/control" {
                        let folder = PathBuf::from(url.strip_prefix("file://").unwrap());
                        std::fs::write(folder.join("exit"), "124\n").unwrap();
                    }
                }
                *kept.lock().unwrap() = Some(document);
            }
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                answer.len()
            );
            stream.write_all(reply.as_bytes()).unwrap();
        }
    });
    let backend = TesBackend {
        run: String::new(),
        client: Client::new(&address, None).unwrap(),
        scratch: folder.path().join("scratch"),
        staging: None,
        settings: Default::default(),
        configs: ImageConfigs::known([("test".into(), ImageConfig::default())]),
    };
    let spec = simple_spec();
    let placement = Placement {
        time_seconds: Some(60),
        ..Placement::default()
    };
    let staging = Staging::default();
    let job = Job {
        name: "run-P-2-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let finished = runtime
        .block_on(backend.run(job, &CancellationToken::new()))
        .expect("an end");
    server.join().unwrap();
    if own {
        assert_eq!(finished.exit_code, Some(124), "the task's own end");
        return;
    }
    assert_eq!(finished.exit_code, Some(143));
    let stderr = std::fs::read_to_string(finished.stderr()).expect("the standard error");
    assert!(
        stderr.ends_with("ran longer than its time of 60 seconds\n"),
        "{stderr}"
    );
    // The wrapper runs under a timer of the time of the task.
    let sent = sent.lock().unwrap().clone().expect("a task");
    let command = &sent["executors"][0]["command"];
    assert_eq!(command[1], "-c");
    assert!(
        command[2].as_str().unwrap().contains("( sleep 60 && kill"),
        "{command}"
    );
    assert_eq!(command[4], "/uwe/wrapper.sh");
}

/// What the server of `keep` knows: the tasks it got and which it cancelled, whether it takes
/// the next task without an answer, and whether its list fails.
#[derive(Default)]
struct Kept {
    tasks: Vec<serde_json::Value>,
    canceled: Vec<bool>,
    lose: bool,
    broken: bool,
}

/// A server that keeps its tasks running until they are cancelled, and lists all of them
/// whatever the filters of the list ask for, as a server may ignore them.
fn keep(kept: Arc<Mutex<Kept>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let address = format!("http://{}", listener.local_addr().expect("an address"));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let (first, body) = request(&mut stream);
            let mut kept = kept.lock().expect("the state");
            let (status, answer) = if first.starts_with("POST /ga4gh/tes/v1/tasks ") {
                kept.tasks.push(serde_json::from_str(&body).expect("JSON"));
                kept.canceled.push(false);
                if kept.lose {
                    continue;
                }
                let id = format!("task-{}", kept.tasks.len() - 1);
                ("200 OK", serde_json::json!({ "id": id }))
            } else if first.starts_with("GET /ga4gh/tes/v1/tasks?") && kept.broken {
                ("500 Internal Server Error", serde_json::json!({}))
            } else if first.starts_with("GET /ga4gh/tes/v1/tasks?") {
                let tasks: Vec<serde_json::Value> = (kept.tasks.iter().enumerate())
                    .map(|(index, task)| {
                        serde_json::json!({"id": format!("task-{index}"), "tags": task["tags"]})
                    })
                    .collect();
                ("200 OK", serde_json::json!({ "tasks": tasks }))
            } else {
                let index: usize = (first.split("tasks/task-").nth(1))
                    .and_then(|rest| rest.split([':', '?', ' ']).next())
                    .and_then(|number| number.parse().ok())
                    .expect("a task");
                if first.contains(":cancel") {
                    kept.canceled[index] = true;
                }
                let state = if kept.canceled[index] {
                    "CANCELED"
                } else {
                    "RUNNING"
                };
                let id = format!("task-{index}");
                ("200 OK", serde_json::json!({ "id": id, "state": state }))
            };
            let answer = answer.to_string();
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{answer}",
                answer.len()
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    address
}

#[test]
fn recovers_lost_submissions() {
    let folder = tempfile::tempdir().unwrap();
    // A task of someone else with the same name, which must stay.
    let foreign = serde_json::json!({"name": "lost", "tags": {"uwe-submission": "other"}});
    let kept = Arc::new(Mutex::new(Kept {
        tasks: vec![foreign],
        canceled: vec![false],
        lose: true,
        ..Kept::default()
    }));
    let backend = TesBackend {
        run: "first".into(),
        client: Client::new(&keep(kept.clone()), None).unwrap(),
        scratch: folder.path().join("scratch"),
        staging: None,
        settings: Default::default(),
        configs: ImageConfigs::known([("test".into(), ImageConfig::default())]),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let spec = simple_spec();
    let (staging, placement) = (Staging::default(), Placement::default());
    let lost = Job {
        name: "lost",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let error = runtime
        .block_on(backend.run(lost, &CancellationToken::new()))
        .expect_err("the answer of the submission is lost");
    assert!(!error.retryable());
    let journal = backend.scratch.join("lost/remote.json");
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
    assert!(record["id"].is_null());
    assert_eq!(record["run"], "first", "the journal names its run");
    // A server that cannot list its tasks leaves the journal, but blocks nothing.
    kept.lock().unwrap().lose = false;
    kept.lock().unwrap().broken = true;
    runtime.block_on(backend.reconcile(&[&backend])).unwrap();
    assert!(journal.exists());
    // A submission this young may still come; recovery leaves it for now.
    kept.lock().unwrap().broken = false;
    runtime.block_on(backend.reconcile(&[&backend])).unwrap();
    assert!(journal.exists(), "a young submission stays");
    // Recovery finds the task by its submission tag, and cancels only that one.
    age(&journal);
    runtime.block_on(backend.reconcile(&[&backend])).unwrap();
    assert!(!journal.exists());
    assert_eq!(kept.lock().unwrap().canceled, [false, true]);
    // A submission that the server never took is lost.
    let unsent = backend.scratch.join("unsent");
    std::fs::create_dir_all(&unsent).unwrap();
    std::fs::File::create(unsent.join("remote.lock")).unwrap();
    record["submission"] = serde_json::json!("unsent");
    std::fs::write(unsent.join("remote.json"), record.to_string()).unwrap();
    age(&unsent.join("remote.json"));
    runtime.block_on(backend.reconcile(&[&backend])).unwrap();
    assert!(!unsent.join("remote.json").exists());
    // An accepted task that already ended reconciles too.
    let address = serve(Arc::new(Mutex::new(None)));
    let backend = TesBackend {
        client: Client::new(&address, None).unwrap(),
        ..backend
    };
    std::fs::write(unsent.join("remote.json"), serde_json::json!({
        "backend": "TES", "server": backend.client.server(), "id": "accepted", "owner": u32::MAX, "started": "",
        "host": std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap() }).to_string()).unwrap();
    runtime
        .block_on(backend.reconcile(&[&backend]))
        .expect("a terminal accepted task reconciles");
    assert!(!unsent.join("remote.json").exists());
    assert!(unsent.exists());
}

#[test]
fn refuses_identity_modes() {
    let folder = tempfile::tempdir().unwrap();
    let source = content_store::Store::memory().unwrap();
    let operator = opendal::Operator::new(opendal::services::Memory::default()).unwrap();
    let objects = content_store::Store::remote(operator, "s3://bucket/stage").unwrap();
    let stage = tes_executor::ObjectStage::new(source, objects, Default::default()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    for staging in [None, Some(stage)] {
        let backend = TesBackend {
            run: String::new(),
            client: Client::new("http://127.0.0.1:1", None).unwrap(),
            scratch: folder.path().join("scratch"),
            staging,
            configs: Default::default(),
            settings: Default::default(),
        };
        let mut spec = simple_spec();
        spec.run_as = Some(task_model::RunAs {
            user: Some(1000),
            group: Some(100),
        });
        let result = runtime.block_on(backend.run(
            Job {
                name: "identity",
                spec: &spec,
                staging: &Staging::default(),
                placement: &Placement::default(),
            },
            &CancellationToken::new(),
        ));
        assert!(matches!(result, Err(ExecutorError::Unsupported(_))));
        assert!(!backend.scratch.exists());
        spec.run_as = None;
        spec.environment.insert(
            "UWE_CONTAINER_FLAGS".into(),
            task_model::EnvironmentValue::Literal("{\"shm\":1073741824}".into()),
        );
        let result = runtime.block_on(backend.run(
            Job {
                name: "shared-memory",
                spec: &spec,
                staging: &Staging::default(),
                placement: &Placement::default(),
            },
            &CancellationToken::new(),
        ));
        assert!(matches!(result, Err(ExecutorError::Unsupported(_))));
        assert!(!backend.scratch.exists());
        spec.environment.clear();
        spec.output_manifest = Some("cwl.output.json".into());
        let result = runtime.block_on(backend.run(
            Job {
                name: "manifest",
                spec: &spec,
                staging: &Staging::default(),
                placement: &Placement::default(),
            },
            &CancellationToken::new(),
        ));
        assert!(matches!(result, Err(ExecutorError::Unsupported(_))));
        assert!(!backend.scratch.exists());
    }
}

#[test]
fn preserves_other_sites() {
    let folder = tempfile::tempdir().unwrap();
    let scratch = folder.path().join("scratch");
    let orphan = scratch.join("other-site");
    std::fs::create_dir_all(&orphan).unwrap();
    std::fs::File::create(orphan.join("remote.lock")).unwrap();
    let journal = serde_json::json!({ "backend":"TES", "server":"http://other/ga4gh/tes/v1", "locked":true, "host":"previous-boot", "id":null });
    std::fs::write(orphan.join("remote.json"), journal.to_string()).unwrap();
    let task = Arc::new(Mutex::new(None));
    let backend = TesBackend {
        run: String::new(),
        client: Client::new(&serve(task.clone()), None).unwrap(),
        scratch,
        staging: None,
        configs: Default::default(),
        settings: Default::default(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime
        .block_on(backend.run(
            Job {
                name: "our-site",
                spec: &simple_spec(),
                staging: &Staging::default(),
                placement: &Placement::default(),
            },
            &CancellationToken::new(),
        ))
        .unwrap();
    assert!(task.lock().unwrap().is_some());
    assert_eq!(
        std::fs::read_to_string(orphan.join("remote.json")).unwrap(),
        journal.to_string()
    );
}

#[test]
fn recovers_dropped_attempts() {
    let folder = tempfile::tempdir().unwrap();
    let scratch = folder.path().join("scratch");
    let orphan = scratch.join("orphan");
    std::fs::create_dir_all(&orphan).unwrap();
    std::fs::File::create(orphan.join("remote.lock")).unwrap();
    let task = Arc::new(Mutex::new(None));
    let address = serve(task.clone());
    let backend = TesBackend {
        run: String::new(),
        client: Client::new(&address, None).unwrap(),
        scratch,
        staging: None,
        settings: Default::default(),
        configs: ImageConfigs::known([("test".into(), ImageConfig::default())]),
    };
    std::fs::write(
        orphan.join("remote.json"),
        serde_json::json!({
            "backend": "TES", "server": backend.client.server(), "name": "orphan",
            "id": "task-1", "owner": std::process::id(), "started": process_start(),
            "host": "previous-boot",
            "locked": true,
        })
        .to_string(),
    )
    .unwrap();
    let spec = simple_spec();
    let (staging, placement) = (Staging::default(), Placement::default());
    let next = Job {
        name: "next",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime
        .block_on(backend.run(next, &CancellationToken::new()))
        .unwrap();
    assert!(!orphan.join("remote.json").exists());
    assert!(task.lock().unwrap().is_some());
}

#[test]
fn preserves_active_attempts() {
    let folder = tempfile::tempdir().unwrap();
    let scratch = folder.path().join("scratch");
    let active = scratch.join("active");
    std::fs::create_dir_all(&active).unwrap();
    let lock = std::fs::File::create(active.join("remote.lock")).unwrap();
    lock.try_lock().unwrap();
    let task = Arc::new(Mutex::new(None));
    let address = serve(task.clone());
    let backend = TesBackend {
        run: String::new(),
        client: Client::new(&address, None).unwrap(),
        scratch,
        staging: None,
        settings: Default::default(),
        configs: ImageConfigs::known([("test".into(), ImageConfig::default())]),
    };
    std::fs::write(
        active.join("remote.json"),
        serde_json::json!({
            "backend": "TES", "server": backend.client.server(), "name": "active",
            "id": "task-1", "owner": std::process::id(), "started": process_start(),
            "host": "foreign-owner-boot",
            "locked": true,
        })
        .to_string(),
    )
    .unwrap();
    let spec = simple_spec();
    let (staging, placement) = (Staging::default(), Placement::default());
    let cancelled = Job {
        name: "cancelled",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let cancel = CancellationToken::new();
    cancel.cancel();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    assert!(matches!(
        runtime.block_on(backend.run(cancelled, &cancel)),
        Err(ExecutorError::Cancelled)
    ));
    assert!(active.join("remote.json").is_file());
    assert!(task.lock().unwrap().is_none());
    let sibling = Job {
        name: "sibling",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    runtime
        .block_on(backend.run(sibling, &CancellationToken::new()))
        .unwrap();
    assert!(active.join("remote.json").is_file());
    assert!(task.lock().unwrap().is_some());
}
