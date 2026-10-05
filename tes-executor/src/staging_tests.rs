//! Exercises object staging through a strict TES server and in-memory S3 objects.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use executor::{Backend, ExecutorError, Job, Staged, Staging};
use opendal::{Operator, services::Memory};
use serde_json::{Value, json};
use task_model::{
    Digest, EnvironmentValue, FORMAT, FileKind, Image, InputFile, OutputRule, Placement, TaskSpec,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::{Client, TesBackend};

#[path = "contracts.rs"]
mod contracts;

struct Server {
    address: String,
    documents: Arc<Mutex<Vec<Value>>>,
    running: Arc<Notify>,
    cancel: CancellationToken,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn server(operator: Operator, objects: Store, outcome: &str) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let documents = Arc::new(Mutex::new(Vec::new()));
    let captured = documents.clone();
    let cancel = CancellationToken::new();
    let stopping = cancel.clone();
    let running = Arc::new(Notify::new());
    let observed = running.clone();
    let outcome = outcome.to_owned();
    tokio::spawn(async move {
        let mut canceled = 0;
        loop {
            let accepted = tokio::select! {
                answer = listener.accept() => answer.unwrap(),
                () = stopping.cancelled() => break,
            };
            let (stream, _) = accepted;
            let mut reader = BufReader::new(stream);
            let mut first = String::new();
            reader.read_line(&mut first).await.unwrap();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                if line.trim().is_empty() {
                    break;
                }
                if let Some((key, value)) = line.split_once(':')
                    && key.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).await.unwrap();
            let answer = if first.starts_with("POST /ga4gh/tes/v1/tasks ") {
                let task: Value = serde_json::from_slice(&body).unwrap();
                enforce(&task);
                for input in task["inputs"].as_array().unwrap() {
                    let key = objects.location_of(input["url"].as_str().unwrap()).unwrap();
                    assert!(operator.exists(&key).await.unwrap());
                }
                let mut records = captured.lock().unwrap();
                let key = &task["tags"]["example.org/idempotency"];
                let index = records.iter().position(|record: &Value| {
                    !key.is_null() && record["tags"]["example.org/idempotency"] == *key
                });
                let index = index.unwrap_or_else(|| {
                    records.push(task);
                    records.len() - 1
                });
                json!({"id": format!("task-{index}")})
            } else if first.contains(":cancel") {
                if canceled == 0 {
                    canceled = 1;
                }
                json!({})
            } else {
                let index: usize = first
                    .split("tasks/task-")
                    .nth(1)
                    .unwrap()
                    .split('?')
                    .next()
                    .unwrap()
                    .parse()
                    .unwrap();
                let task = captured.lock().unwrap()[index].clone();
                let state = if canceled > 0 {
                    let state = if canceled == 1 {
                        "CANCELING"
                    } else {
                        "CANCELED"
                    };
                    canceled += 1;
                    state
                } else if outcome == "retry" && index == 0 {
                    "SYSTEM_ERROR"
                } else if outcome == "retry" {
                    "COMPLETE"
                } else {
                    &outcome
                };
                if state == "RUNNING" {
                    observed.notify_one();
                }
                let outputs = if state == "COMPLETE" {
                    leave(&operator, &objects, &task).await
                } else {
                    Vec::new()
                };
                json!({"id": format!("task-{index}"), "state": state, "logs": [{
                    "logs": [{"exit_code": if state == "EXECUTOR_ERROR" { 17 } else { 0 }, "stdout": "server tail", "stderr": "server error"}],
                    "outputs": outputs, "system_logs": ["infrastructure error"]
                }]})
            };
            let body = answer.to_string();
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            reader.get_mut().write_all(reply.as_bytes()).await.unwrap();
        }
    });
    Server {
        address,
        documents,
        running,
        cancel,
    }
}

fn enforce(task: &Value) {
    let executors = task["executors"].as_array().unwrap();
    assert_eq!(executors.len(), 1);
    assert!(!executors[0]["command"].as_array().unwrap().is_empty());
    for field in ["stdin", "stdout", "stderr"] {
        assert!(executors[0].get(field).is_none());
    }
    assert!(task.get("volumes").is_none());
    let inputs = task["inputs"].as_array().unwrap();
    let outputs = task["outputs"].as_array().unwrap();
    assert!(inputs.len() <= 512 && outputs.len() <= 1024);
    for file in inputs.iter().chain(outputs) {
        assert_eq!(file["type"], "FILE");
        assert!(file["url"].as_str().unwrap().starts_with("s3://bucket/"));
        assert!(file.get("content").is_none());
        let path = file["path"].as_str().unwrap();
        assert!(
            path.starts_with('/')
                && !path[1..]
                    .split('/')
                    .any(|part| part.is_empty() || part == "." || part == "..")
        );
    }
    for output in outputs {
        let pattern = output["path"].as_str().unwrap();
        if wildcard(pattern) {
            let prefix = output["path_prefix"].as_str().unwrap();
            assert!(
                prefix.starts_with('/')
                    && prefix != "/"
                    && !prefix[1..]
                        .split('/')
                        .any(|part| part.is_empty() || part == "." || part == "..")
            );
            assert!(pattern.starts_with(&format!("{prefix}/")));
        }
    }
    for input in inputs {
        let path = input["path"].as_str().unwrap();
        assert!(!wildcard(path));
        for output in outputs {
            let pattern = output["path"].as_str().unwrap();
            if wildcard(pattern) {
                assert!(!glob::Pattern::new(pattern).unwrap().matches(path));
            } else {
                assert!(
                    !Path::new(path).starts_with(pattern) && !Path::new(pattern).starts_with(path)
                );
            }
        }
    }
}

async fn leave(operator: &Operator, objects: &Store, task: &Value) -> Vec<Value> {
    let mut logs = Vec::new();
    for output in task["outputs"].as_array().unwrap() {
        let path = output["path"].as_str().unwrap();
        let url = output["url"].as_str().unwrap();
        let entries: Vec<(&str, &str)> = match path {
            "/iunctus/control/exit" => vec![("", "0\n")],
            "/iunctus/control/manifest" => {
                vec![("", "file out/a.txt\ndirectory tree\ndirectory empty\n")]
            }
            "/iunctus/control/objects" => vec![("", "1 ./out/a.txt\n2 ./tree/deep/.hidden\n")],
            "/iunctus/control/stdout" => vec![("", "captured output\n")],
            "/iunctus/control/stderr" => vec![("", "")],
            "/iunctus/control/start" => vec![("", "1700000000\n")],
            "/iunctus/control/end" => vec![("", "1700000001\n")],
            "/iunctus/control/usage" => vec![("", "")],
            "/iunctus/transfer/*" => vec![("1", "alpha beta\n"), ("2", "hidden\n")],
            _ => vec![("a.txt", "alpha beta\n")],
        };
        for (suffix, bytes) in entries {
            let address = format!("{url}{suffix}");
            let key = objects.location_of(&address).unwrap();
            operator
                .write(&key, bytes.as_bytes().to_vec())
                .await
                .unwrap();
            let path = if suffix.is_empty() {
                path.into()
            } else {
                format!("{}/{suffix}", output["path_prefix"].as_str().unwrap())
            };
            logs.push(json!({"url": address, "path": path, "size_bytes": bytes.len().to_string()}));
        }
    }
    logs
}

fn spec(shell: bool) -> TaskSpec {
    TaskSpec {
        format: FORMAT,
        namespace: String::new(),
        image: Some(Image {
            reference: "test@sha256:fixed".into(),
            platform: None,
        }),
        arguments: if shell {
            vec![
                "sh".into(),
                "-c".into(),
                "cat in.txt directory/nested/b.txt > out/a.txt".into(),
            ]
        } else {
            vec!["tool".into(), "in.txt".into()]
        },
        replaces_entrypoint: true,
        workdir: None,
        run_as: None,
        environment: BTreeMap::from([("WORK".into(), EnvironmentValue::WorkDirectory)]),
        inputs: Vec::new(),
        outputs: vec![OutputRule {
            pattern: "out/*.txt".into(),
        }],
        stdin: None,
        containers: Vec::new(),
    }
}

fn stores() -> (Operator, Store, Store) {
    let operator = Operator::new(Memory::default()).unwrap();
    let objects = Store::remote(operator.clone(), "s3://bucket/tes").unwrap();
    (operator, objects, Store::memory().unwrap())
}

fn backend(server: &Server, root: &Path, source: &Store, objects: &Store) -> TesBackend {
    let settings = Settings {
        idempotency_tag: Some("example.org/idempotency".into()),
        ..Settings::default()
    };
    TesBackend {
        client: Client::new(&server.address, None).unwrap(),
        scratch: root.join("scratch"),
        configs: Default::default(),
        staging: Some(ObjectStage::new(source.clone(), objects.clone(), settings.clone()).unwrap()),
        settings,
    }
}

#[tokio::test]
async fn restores_object_outputs() {
    let folder = tempfile::tempdir().unwrap();
    let (operator, objects, source) = stores();
    let server = server(operator.clone(), objects.clone(), "COMPLETE").await;
    let mut backend = backend(&server, folder.path(), &source, &objects);
    backend.configs = executor::registry::ImageConfigs::known([(
        "test@sha256:fixed".into(),
        executor::registry::ImageConfig {
            entrypoint: vec!["/entry".into()],
            ..Default::default()
        },
    )]);
    let input = folder.path().join("input");
    let directory = folder.path().join("directory");
    std::fs::create_dir_all(directory.join("nested")).unwrap();
    std::fs::write(&input, b"alpha").unwrap();
    std::fs::write(directory.join("nested/b.txt"), b"beta").unwrap();
    let mut spec = spec(true);
    spec.replaces_entrypoint = false;
    let digest = source.put_content(b"beta".to_vec().into()).await.unwrap();
    let tree = task_model::Tree {
        entries: vec![task_model::TreeEntry::File {
            path: "nested/b.txt".into(),
            digest,
            size: 4,
            executable: false,
        }],
    };
    let digest = source.put_tree(&tree).await.unwrap();
    spec.inputs.push(InputFile {
        path: "directory".into(),
        kind: FileKind::Directory,
        digest,
        executable: false,
        writable: false,
    });
    spec.inputs.push(InputFile {
        path: "in.txt".into(),
        kind: FileKind::File,
        digest: Digest::sha256(b"alpha"),
        executable: false,
        writable: false,
    });
    spec.outputs.extend([
        OutputRule {
            pattern: "tree/".into(),
        },
        OutputRule {
            pattern: "empty/".into(),
        },
    ]);
    let staging = Staging {
        inputs: vec![
            Staged {
                path: "in.txt".into(),
                source: input,
                ..Staged::default()
            },
            Staged {
                path: "directory".into(),
                source: directory,
                directory: true,
                ..Staged::default()
            },
        ],
        ..Staging::default()
    };
    let placement = Placement::default();
    let job = Job {
        name: "run-task-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let finished = backend.run(job, &CancellationToken::new()).await.unwrap();
    assert_eq!(finished.exit_code, Some(0));
    assert_eq!(
        std::fs::read(finished.local_path("out/a.txt")).unwrap(),
        b"alpha beta\n"
    );
    assert_eq!(
        std::fs::read(finished.local_path("tree/deep/.hidden")).unwrap(),
        b"hidden\n"
    );
    assert!(finished.local_path("empty").is_dir());
    assert_eq!(
        source
            .get_content(&Digest::sha256(b"alpha beta\n"))
            .await
            .unwrap()
            .as_ref(),
        b"alpha beta\n"
    );
    assert_eq!(
        std::fs::read(finished.stdout()).unwrap(),
        b"captured output\n"
    );
    let task = server.documents.lock().unwrap()[0].clone();
    assert_eq!(
        task["inputs"][1]["path"],
        "/iunctus/work/directory/nested/b.txt"
    );
    for (index, bytes) in [(0, b"alpha".as_slice()), (1, b"beta".as_slice())] {
        let key = objects
            .location_of(task["inputs"][index]["url"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            operator.read(&key).await.unwrap().to_bytes().as_ref(),
            bytes
        );
    }
    let first = backend
        .client
        .create(&serde_json::from_value(task.clone()).unwrap())
        .await
        .unwrap();
    let second = backend
        .client
        .create(&serde_json::from_value(task).unwrap())
        .await
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(server.documents.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn preserves_direct_argv() {
    let folder = tempfile::tempdir().unwrap();
    let (operator, objects, source) = stores();
    let server = server(operator, objects.clone(), "COMPLETE").await;
    let backend = backend(&server, folder.path(), &source, &objects);
    let spec = spec(false);
    let finished = backend
        .run(
            Job {
                name: "direct-1",
                spec: &spec,
                staging: &Staging::default(),
                placement: &Placement::default(),
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(finished.files[0].path, "out/a.txt");
    assert_eq!(
        server.documents.lock().unwrap()[0]["executors"][0]["command"],
        json!(spec.arguments)
    );
    assert_eq!(std::fs::read(finished.stdout()).unwrap(), b"server tail");
}

#[tokio::test]
async fn executor_error_permanent() {
    let folder = tempfile::tempdir().unwrap();
    let (operator, objects, source) = stores();
    let server = server(operator, objects.clone(), "EXECUTOR_ERROR").await;
    let backend = backend(&server, folder.path(), &source, &objects);
    let spec = spec(false);
    let finished = backend
        .run(
            Job {
                name: "failure-1",
                spec: &spec,
                staging: &Staging::default(),
                placement: &Placement::default(),
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(finished.exit_code, Some(17));
    assert!(finished.files.is_empty());
}

#[tokio::test]
async fn retries_system_error() {
    let folder = tempfile::tempdir().unwrap();
    let (operator, objects, source) = stores();
    let server = server(operator, objects.clone(), "retry").await;
    let backend = backend(&server, folder.path(), &source, &objects);
    let spec = spec(false);
    let staging = Staging::default();
    let placement = Placement::default();
    let job = Job {
        name: "retry-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    assert!(
        backend
            .run(job, &CancellationToken::new())
            .await
            .unwrap_err()
            .retryable()
    );
    assert_eq!(
        backend
            .run(
                Job {
                    name: "retry-2",
                    ..job
                },
                &CancellationToken::new()
            )
            .await
            .unwrap()
            .exit_code,
        Some(0)
    );
    let tasks = server.documents.lock().unwrap();
    assert_ne!(
        tasks[0]["tags"]["example.org/idempotency"],
        tasks[1]["tags"]["example.org/idempotency"]
    );
}

#[tokio::test]
async fn waits_for_cancellation() {
    let folder = tempfile::tempdir().unwrap();
    let (operator, objects, source) = stores();
    let server = server(operator, objects.clone(), "RUNNING").await;
    let backend = backend(&server, folder.path(), &source, &objects);
    let spec = spec(false);
    let staging = Staging::default();
    let placement = Placement::default();
    let cancel = CancellationToken::new();
    let job = Job {
        name: "cancel-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let stop = async {
        server.running.notified().await;
        cancel.cancel();
    };
    let (result, ()) = tokio::join!(backend.run(job, &cancel), stop);
    assert!(matches!(result, Err(ExecutorError::Cancelled)));
    assert!(!backend.scratch.join(job.name).join("remote.json").exists());
}
