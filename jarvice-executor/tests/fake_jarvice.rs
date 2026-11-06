//! Checks the JARVICE backend against a local fake of the JARVICE API that runs each job script
//! here: the submitted body, the exit code of a failed task, and the end of a cancelled job.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use base64::Engine;
use executor::{Backend, ExecutorError, Finished, Job, Staged, Staging};
use jarvice_executor::{Client, JarviceBackend, Settings};
use serde_json::json;
use task_model::{FORMAT, Image, OutputRule, Placement, TaskSpec};
use tokio_util::sync::CancellationToken;

use common::FakeJarvice;

/// An image with a digest, which needs no registry.
const IMAGE: &str =
    "ubuntu@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A backend whose jobs see `folder` as their vault, with the machine type `n0`.
fn backend(folder: &Path, fake: &FakeJarvice) -> JarviceBackend {
    let client = Client::new(&fake.address, "probeuser".into(), "not-a-real-key".into());
    let settings = Settings {
        machine_type: Some("n0".into()),
        vault: Some("persistent".into()),
        vault_path: folder.to_string_lossy().into_owned(),
        poll: Duration::from_millis(50),
        ..Settings::default()
    };
    let scratch = folder.join("scratch");
    let mut backend =
        JarviceBackend::new(client, settings, scratch, None, None).expect("a backend");
    backend.staging_time = Duration::ZERO;
    backend
}

fn spec(script: &str) -> TaskSpec {
    TaskSpec {
        output_manifest: None,
        run_as: None,
        format: FORMAT,
        namespace: String::new(),
        image: Some(Image {
            reference: IMAGE.into(),
            platform: None,
        }),
        arguments: vec!["/bin/sh".into(), "-c".into(), script.into()],
        workdir: None,
        environment: BTreeMap::new(),
        inputs: Vec::new(),
        outputs: vec![OutputRule {
            pattern: "out.txt".into(),
        }],
        stdin: None,
        containers: Vec::new(),
        replaces_entrypoint: false,
    }
}

async fn run(
    backend: &JarviceBackend,
    name: &str,
    spec: &TaskSpec,
    placement: &Placement,
    staging: &Staging,
    cancel: &CancellationToken,
) -> Result<Finished, ExecutorError> {
    let job = Job {
        name,
        spec,
        staging,
        placement,
    };
    backend.run(job, cancel).await
}

/// A task runs as a batch job with the fields of the JARVICE API: its machine type from its
/// `machineType` option, the vault, the user with its key, and the job script in base64.
#[tokio::test]
async fn submits_batch_jobs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeJarvice::start();
    let backend = backend(folder.path(), &fake);
    let source = folder.path().join("in.txt");
    std::fs::write(&source, "hello\n").expect("writes the input");
    let staging = Staging {
        inputs: vec![Staged {
            path: "in.txt".into(),
            source,
            ..Staged::default()
        }],
        ..Staging::default()
    };
    let placement = Placement {
        options: [("machineType".to_owned(), json!("n1"))].into(),
        ..Placement::default()
    };
    let cancel = CancellationToken::new();
    let upper = spec("tr a-z A-Z < in.txt > out.txt");
    let finished = run(
        &backend,
        "run-BIG-1-1",
        &upper,
        &placement,
        &staging,
        &cancel,
    )
    .await;
    let finished = finished.expect("the job ends");
    assert_eq!(finished.exit_code, Some(0));
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("the output");
    assert_eq!(output, "HELLO\n");
    let submitted = fake.requests("POST /jarvice/batch");
    assert_eq!(submitted.len(), 1);
    let body = &submitted[0].body;
    assert_eq!(body["machine"], json!({"type": "n1", "nodes": 1}));
    let vault = json!({"name": "persistent", "readonly": false, "force": false});
    assert_eq!(body["vault"], vault);
    let user = json!({"username": "probeuser", "apikey": "<masked>"});
    assert_eq!(body["user"], user);
    assert_eq!(body["job_label"], "uwe-run-BIG-1-1");
    assert_eq!(body["container"]["image"], format!("library/{IMAGE}"));
    let encoded = body["container"]["jobscript"]
        .as_str()
        .expect("a job script");
    let script = base64::engine::general_purpose::STANDARD.decode(encoded);
    let script = String::from_utf8(script.expect("base64")).expect("text");
    assert!(script.contains("/control/wrapper.sh"), "{script}");
    let status = fake.requests("GET /jarvice/status?");
    assert!(!status.is_empty());
    assert!(
        status
            .iter()
            .all(|request| request.path.contains("apikey=<masked>")),
        "the key goes in the query"
    );
    let (failed, empty) = (spec("echo broken >&2; exit 3"), Staging::default());
    let failed = run(
        &backend,
        "run-BIG-2-1",
        &failed,
        &placement,
        &empty,
        &cancel,
    )
    .await;
    assert_eq!(failed.expect("the job ends").exit_code, Some(3));
}

/// A task without a machine type is refused before any job starts, and a cancelled task ends
/// its job.
#[tokio::test]
async fn cancels_running_jobs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeJarvice::start();
    let mut backend = backend(folder.path(), &fake);
    backend.settings.machine_type = None;
    let cancel = CancellationToken::new();
    let (placement, staging) = (Placement::default(), Staging::default());
    let refused = run(
        &backend,
        "run-A-1-1",
        &spec("true"),
        &placement,
        &staging,
        &cancel,
    )
    .await;
    assert!(
        matches!(refused, Err(ExecutorError::Unsupported(_))),
        "{refused:?}"
    );
    assert!(fake.requests("POST").is_empty());
    backend.settings.machine_type = Some("n0".into());
    let stopper = cancel.clone();
    let watch = async {
        while !fake.running(9001) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        stopper.cancel();
    };
    let long = spec("sleep 600");
    let (ended, ()) = tokio::join!(
        run(&backend, "run-A-2-1", &long, &placement, &staging, &cancel),
        watch
    );
    assert!(matches!(ended, Err(ExecutorError::Cancelled)), "{ended:?}");
    assert_eq!(fake.requests("GET /jarvice/shutdown").len(), 1);
    assert!(!fake.running(9001));
}
