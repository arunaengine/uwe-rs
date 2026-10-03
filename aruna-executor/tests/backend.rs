//! Native jobs, immutable bundle transfer and content-store collection against a fake server.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, Read, Write};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use aruna_executor::{
    ArunaBackend,
    client::{Client, Object},
    objects::{Objects, Reference, Transfer},
};
use executor::{Backend, CancellationToken, ExecutorError, Job, Staging};
use serde_json::{Value, json};
use task_model::{Digest, Image, OutputRule, Placement, TaskSpec};

const ID: &str = "01JJRSTVWXYZ0123456789ABCD";

struct Memory {
    result: Vec<u8>,
    writes: AtomicUsize,
    reads: AtomicUsize,
}

impl Objects for Memory {
    fn write<'a>(
        &'a self,
        group: &'a str,
        key: &'a str,
        file: &'a std::path::Path,
    ) -> Transfer<'a, Reference> {
        Box::pin(async move {
            assert_eq!(group, ID);
            self.writes.fetch_add(1, Ordering::SeqCst);
            let bytes = std::fs::read(file)?;
            let mut paths = Vec::new();
            for entry in tar::Archive::new(bytes.as_slice()).entries()? {
                paths.push(entry?.path()?.to_string_lossy().into_owned());
            }
            assert!(paths.contains(&"wrapper.sh".into()) && paths.contains(&"start.sh".into()));
            Ok(Reference {
                bucket: "inputs".into(),
                key: key.into(),
                version_id: Digest::sha256(&bytes).to_string(),
            })
        })
    }

    fn read<'a>(&'a self, object: &'a Object, maximum: u64) -> Transfer<'a, Vec<u8>> {
        Box::pin(async move {
            assert_eq!(object.version_id, "immutable-output-version");
            assert_eq!(
                object.endpoint_url.as_deref(),
                Some("https://authorized-owner.example/s3")
            );
            assert!(self.result.len() as u64 <= maximum);
            self.reads.fetch_add(1, Ordering::SeqCst);
            Ok(self.result.clone())
        })
    }
}

struct CancelRead {
    memory: Memory,
    cancel: CancellationToken,
}

impl Objects for CancelRead {
    fn write<'a>(
        &'a self,
        group: &'a str,
        key: &'a str,
        file: &'a std::path::Path,
    ) -> Transfer<'a, Reference> {
        self.memory.write(group, key, file)
    }

    fn read<'a>(&'a self, object: &'a Object, maximum: u64) -> Transfer<'a, Vec<u8>> {
        Box::pin(async move {
            self.cancel.cancel();
            self.memory.read(object, maximum).await
        })
    }
}

#[derive(Clone, Copy)]
enum Case {
    Success,
    Failure,
    Cancel,
    LateSuccess,
    Partial,
}

struct Server {
    url: String,
    done: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    plans: Arc<Mutex<Vec<Value>>>,
    cancellations: Arc<AtomicUsize>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}

fn server(case: Case, size: u64, cancel: CancellationToken) -> Server {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/api/v1", listener.local_addr().unwrap());
    let done = Arc::new(AtomicBool::new(false));
    let plans = Arc::new(Mutex::new(Vec::new()));
    let cancellations = Arc::new(AtomicUsize::new(0));
    let (finished, submissions, stops) = (done.clone(), plans.clone(), cancellations.clone());
    let worker = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut reads = 0;
        let mut after_cancel = 0;
        while !finished.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "fake server timed out");
            let (mut stream, _) = match listener.accept() {
                Ok(stream) => stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(error) => panic!("{error}"),
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = std::io::BufReader::new(&mut stream);
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            drop(reader);
            let (code, body) = if first.starts_with("POST /api/v1/compute/jobs ") {
                let plan: Value = serde_json::from_slice(&bytes).unwrap();
                let mut plans = submissions.lock().unwrap();
                let created = plans.is_empty();
                if let Some(previous) = plans.first() {
                    assert_eq!(previous, &plan);
                }
                plans.push(plan);
                if matches!(case, Case::Cancel | Case::LateSuccess) {
                    cancel.cancel();
                }
                (
                    if created { 201 } else { 200 },
                    json!({"job_id": ID, "created": created,
                    "canonical_job_id": ID, "state": "queued", "origin_node_url": "https://unused.example",
                    "status_url": "https://unused.example/status"}),
                )
            } else {
                let cancelling =
                    first.starts_with(&format!("POST /api/v1/compute/jobs/{ID}/cancel "));
                assert!(
                    cancelling || first.starts_with(&format!("GET /api/v1/compute/jobs/{ID} "))
                );
                if cancelling {
                    stops.fetch_add(1, Ordering::SeqCst);
                } else {
                    reads += 1;
                    if stops.load(Ordering::SeqCst) > 0 {
                        after_cancel += 1;
                    }
                }
                let state = match case {
                    Case::Failure => "failed",
                    Case::Cancel if after_cancel >= 2 => "cancelled",
                    Case::LateSuccess if after_cancel >= 2 => "succeeded",
                    Case::Cancel | Case::LateSuccess => "running",
                    _ => "succeeded",
                };
                let partial = matches!(case, Case::Partial) && reads == 1;
                let plans = submissions.lock().unwrap();
                let output = &plans[0]["outputs"][0];
                (
                    if cancelling { 202 } else { 200 },
                    json!({"job_id": ID, "kind": "execution",
                    "state": state, "attempts": 1, "cancel_requested": stops.load(Ordering::SeqCst) > 0,
                    "created_at": "2026-10-08T09:00:00Z", "updated_at": "2026-10-08T09:00:01Z",
                    "progress": {"current": 1, "unit": "phases"}, "workspace_mode": "none",
                    "error": if matches!(case, Case::Failure) { json!({"kind": "executor", "message": "fixture failure reason"}) } else { Value::Null },
                    "family": {"submission_id": "submission", "request_digest": "request", "canonical_job_id": ID,
                        "aliases": [ID], "alias_count": 1, "conflict_count": 0, "logical_state": state,
                        "canonical_execution_id": "execution", "executions": 1, "execution_list": [],
                        "duplicate_successes": 0, "revision": 1, "projection_digest": "projection",
                        "partial": partial, "locally_exhausted": partial, "cancel_requested": stops.load(Ordering::SeqCst) > 0,
                        "outputs": [{"bucket": output["bucket"], "key": output["dest_key"], "version_id": "immutable-output-version",
                            "execution_id": "execution", "container_path": output["container_path"], "size": size,
                            "endpoint_url": "https://authorized-owner.example/s3"}]}}),
                )
            };
            let body = body.to_string();
            write!(
                stream,
                "HTTP/1.1 {code} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    Server {
        url,
        done,
        worker: Some(worker),
        plans,
        cancellations,
    }
}

fn result(exit: i32) -> Vec<u8> {
    let mut archive = tar::Builder::new(Vec::new());
    for (path, bytes) in [
        ("control/exit", exit.to_string()),
        (
            "control/manifest",
            "file output.txt\ndirectory folder\n".into(),
        ),
        ("control/stdout", "fixture stdout".into()),
        ("control/stderr", "fixture stderr".into()),
        ("outbox/output.txt", "output bytes".into()),
        ("outbox/folder/nested.txt", "nested bytes".into()),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, path, bytes.as_bytes())
            .unwrap();
    }
    archive.into_inner().unwrap()
}

fn spec() -> TaskSpec {
    TaskSpec {
        format: task_model::FORMAT,
        namespace: "test".into(),
        image: Some(Image {
            reference: format!("image@sha256:{}", "a".repeat(64)),
            platform: None,
        }),
        arguments: vec!["true".into()],
        workdir: None,
        environment: Default::default(),
        inputs: Vec::new(),
        outputs: vec![
            OutputRule {
                pattern: "output.txt".into(),
            },
            OutputRule {
                pattern: "folder/".into(),
            },
        ],
        stdin: None,
        containers: Vec::new(),
        replaces_entrypoint: true,
        run_as: None,
    }
}

fn backend(server: &Server, root: &std::path::Path, objects: Arc<Memory>) -> ArunaBackend {
    ArunaBackend {
        client: Client::new(&server.url, Some("fixture-secret".into())).unwrap(),
        scratch: root.join("aruna"),
        scope: "caller-test".into(),
        group_id: ID.into(),
        output_bucket: "results".into(),
        objects,
        configs: Default::default(),
    }
}

#[tokio::test]
async fn success_and_idempotent_resubmit_reuse_the_exact_plan_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let bytes = result(0);
    let memory = Arc::new(Memory {
        result: bytes.clone(),
        writes: AtomicUsize::new(0),
        reads: AtomicUsize::new(0),
    });
    let cancel = CancellationToken::new();
    let server = server(Case::Success, bytes.len() as u64, cancel.clone());
    let spec = spec();
    let staging = Staging::default();
    let placement = Placement {
        cpus: 2,
        memory_bytes: Some(4096),
        time_seconds: Some(30),
        ..Placement::default()
    };
    for name in ["first", "second"] {
        let finished = backend(&server, root.path(), memory.clone())
            .run(
                Job {
                    name,
                    spec: &spec,
                    staging: &staging,
                    placement: &placement,
                },
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(finished.exit_code, Some(0));
        assert_eq!(
            std::fs::read(finished.local_path("output.txt")).unwrap(),
            b"output bytes"
        );
        assert_eq!(
            std::fs::read(finished.local_path("folder/nested.txt")).unwrap(),
            b"nested bytes"
        );
        std::fs::remove_dir_all(finished.root).unwrap();
    }
    assert_eq!(memory.writes.load(Ordering::SeqCst), 1);
    assert_eq!(server.plans.lock().unwrap().len(), 2);
    {
        let plans = server.plans.lock().unwrap();
        assert_eq!(plans[0]["cpu_cores"], 2);
        assert_eq!(plans[0]["ram_bytes"], 4096);
        assert_eq!(plans[0]["max_walltime_ms"], 30_000);
        assert_eq!(plans[0]["inputs"][0]["mode"], "exact_reference");
    }
    let changed = Placement {
        cpus: 3,
        ..placement
    };
    let error = backend(&server, root.path(), memory)
        .run(
            Job {
                name: "changed",
                spec: &spec,
                staging: &staging,
                placement: &changed,
            },
            &cancel,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("submission plan changed"));
}

#[tokio::test]
async fn failure_preserves_the_server_reason() {
    let root = tempfile::tempdir().unwrap();
    let bytes = result(0);
    let memory = Arc::new(Memory {
        result: bytes.clone(),
        writes: AtomicUsize::new(0),
        reads: AtomicUsize::new(0),
    });
    let cancel = CancellationToken::new();
    let server = server(Case::Failure, bytes.len() as u64, cancel.clone());
    let error = backend(&server, root.path(), memory.clone())
        .run(
            Job {
                name: "failure",
                spec: &spec(),
                staging: &Staging::default(),
                placement: &Placement::default(),
            },
            &cancel,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("fixture failure reason"));
    assert_eq!(memory.reads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancellation_waits_past_intent_and_preserves_late_success() {
    for case in [Case::Cancel, Case::LateSuccess] {
        let root = tempfile::tempdir().unwrap();
        let bytes = result(0);
        let memory = Arc::new(Memory {
            result: bytes.clone(),
            writes: AtomicUsize::new(0),
            reads: AtomicUsize::new(0),
        });
        let cancel = CancellationToken::new();
        let server = server(case, bytes.len() as u64, cancel.clone());
        let error = backend(&server, root.path(), memory.clone())
            .run(
                Job {
                    name: "cancel",
                    spec: &spec(),
                    staging: &Staging::default(),
                    placement: &Placement::default(),
                },
                &cancel,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ExecutorError::Cancelled));
        assert_eq!(server.cancellations.load(Ordering::SeqCst), 2);
        assert_eq!(memory.reads.load(Ordering::SeqCst), 0);
        let record = std::fs::read_dir(root.path().join("aruna/journals"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("request.json");
        let journal: Value = serde_json::from_slice(&std::fs::read(record).unwrap()).unwrap();
        assert_eq!(journal["cancel_pending"], true);
        assert_eq!(journal["id"], ID);
        assert_eq!(
            journal["terminal"],
            if matches!(case, Case::LateSuccess) {
                "succeeded"
            } else {
                "cancelled"
            }
        );
    }
}
