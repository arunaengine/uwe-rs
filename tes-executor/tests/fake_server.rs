//! The TES backend against a small fake server on this machine, which answers as Funnel does
//! and writes the files that the task would give back.
// Copyright (c) 2026 The iunctus Contributors
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
        Some("/iunctus/control") => {
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
    assert_eq!(sent["executors"][0]["command"][1], "/iunctus/wrapper.sh");
    let wrapper = std::fs::read_to_string(finished.root.join("wrapper.sh")).expect("reads");
    assert!(
        wrapper.contains("'/usr/local/env-execute' 'bash' '-c' 'cp in.txt out.txt'"),
        "{wrapper}"
    );
    assert_eq!(sent["inputs"][1]["path"], "/iunctus/work/in.txt");
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
    for timed_out in [false, true] {
        let folder = tempfile::tempdir().expect("a folder");
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let address = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let server = std::thread::spawn(move || {
            let answers = if timed_out {
                vec![
                    (200, r#"{"id":"task-1"}"#),
                    (200, r#"{"id":"task-1","state":"RUNNING"}"#),
                    (200, "{}"),
                    (200, r#"{"id":"task-1","state":"CANCELED"}"#),
                ]
            } else {
                vec![
                    (200, r#"{"id":"task-1"}"#),
                    (503, "unavailable"),
                    (200, r#"{"id":"task-1","state":"RUNNING"}"#),
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
        let backend = TesBackend {
            client: Client::new(&address, None).unwrap(),
            scratch: folder.path().join("scratch"),
            staging: None,
            settings: Default::default(),
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
    }
}

#[test]
fn retains_ambiguous_submissions() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("scratch/attempt");
    std::fs::create_dir_all(&root).unwrap();
    let backend = TesBackend {
        client: Client::new("http://localhost:1", None).unwrap(),
        scratch: folder.path().join("scratch"),
        staging: None,
        settings: Default::default(),
        configs: ImageConfigs::default(),
    };
    std::fs::write(root.join("remote.json"), serde_json::json!({
        "backend": "TES", "server": backend.client.server(), "id": null, "owner": u32::MAX, "started": "",
        "host": std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap() }).to_string()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let error = runtime
        .block_on(backend.reconcile(&[&backend]))
        .expect_err("unknown remote ID blocks recovery");
    assert!(!error.retryable());
    assert!(
        error
            .to_string()
            .contains("submission response was not recorded")
    );
    assert!(root.join("remote.json").exists());
    let spec = simple_spec();
    let (staging, placement) = (Staging::default(), Placement::default());
    let blocked = Job {
        name: "blocked",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let error = runtime
        .block_on(backend.run(blocked, &CancellationToken::new()))
        .expect_err("ambiguous ownership blocks admission");
    assert!(
        error
            .to_string()
            .contains("submission response was not recorded")
    );
    assert!(!backend.scratch.join("blocked").exists());
    let address = serve(Arc::new(Mutex::new(None)));
    let backend = TesBackend {
        client: Client::new(&address, None).unwrap(),
        ..backend
    };
    std::fs::write(root.join("remote.json"), serde_json::json!({
        "backend": "TES", "server": backend.client.server(), "id": "accepted", "owner": u32::MAX, "started": "",
        "host": std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap() }).to_string()).unwrap();
    runtime
        .block_on(backend.reconcile(&[&backend]))
        .expect("a terminal accepted task reconciles");
    assert!(!root.join("remote.json").exists());
    assert!(root.exists());
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
            "host": std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap(),
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
            "host": std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap(),
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
