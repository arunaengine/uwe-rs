//! Checks the Flyte backend against a local fake of Flyte's Connect API that runs each run's
//! container here: tokens from an API key, runs, failures with their log, and aborts.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use executor::batch::Helper;
use executor::remote::RemoteStage;
use executor::{Backend, ExecutorError, Job, Staged, Staging};
use flyte_executor::{ApiKey, Client, FlyteBackend, Project, Settings};
use task_model::{FORMAT, Image, OutputRule, Placement, TaskSpec};
use tokio_util::sync::CancellationToken;

use common::FakeFlyte;

/// An image with a digest, which needs no registry.
const IMAGE: &str =
    "ubuntu@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The test program as the helper of the runs: it runs `stage_helper` with the arguments.
fn helper(folder: &Path) -> std::path::PathBuf {
    let program = std::env::current_exe().expect("this program");
    let script = format!(
        "#!/bin/sh\nIFS=$(printf '\\037')\nUWE_TEST_STAGE=\"$*\" exec '{}' --exact \
         stage_helper --nocapture --test-threads=1\n",
        program.to_string_lossy().replace('\'', r"'\''")
    );
    let path = folder.join("helper");
    std::fs::create_dir_all(folder).expect("makes the folder");
    std::fs::write(&path, script).expect("writes the helper");
    path
}

#[test]
fn stage_helper() {
    if let Ok(arguments) = std::env::var("UWE_TEST_STAGE") {
        let arguments: Vec<String> = arguments.split('\u{1f}').map(str::to_owned).collect();
        // The first argument is `__stage`, as the uwe program takes it.
        executor::helper::main(&arguments[1..]).expect("the helper stages");
    }
}

/// A backend whose calls go to the fake, through a staging root in `folder`, with the helper
/// that Flyte downloads; `sha256` is the digest that the job checks.
fn backend(folder: &Path, fake: &FakeFlyte, sha256: Option<&str>) -> FlyteBackend {
    let helper = helper(folder);
    let bytes = std::fs::read(&helper).expect("reads the helper");
    let digest = task_model::Digest::sha256(&bytes).hex().to_owned();
    let settings = Settings {
        poll: Duration::from_millis(50),
        task_folder: folder.join("node").to_string_lossy().into_owned(),
        helper: Some(Helper::Download {
            url: helper.to_string_lossy().into_owned(),
            sha256: sha256.unwrap_or(&digest).to_owned(),
        }),
        ..Settings::default()
    };
    let key = ApiKey::parse(&fake.api_key("lab")).expect("a key");
    let mut client = Client::new(&fake.address, Some(key));
    client.pause = Duration::from_millis(10);
    let stage = RemoteStage {
        objects: content_store::Store::local(&folder.join("staging")).expect("a store"),
        expiry: Duration::from_secs(600),
        max_outputs: 4,
        credentials: false,
        cleanup: true,
    };
    let project = Project {
        org: "lab".into(),
        project: "flows".into(),
        domain: "development".into(),
    };
    let scratch = folder.join("scratch");
    let mut backend =
        FlyteBackend::new(client, project, settings, scratch, stage, None).expect("a backend");
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
    backend: &FlyteBackend,
    name: &str,
    spec: &TaskSpec,
    staging: &Staging,
) -> Result<executor::Finished, ExecutorError> {
    let placement = Placement {
        cpus: 2,
        memory_bytes: Some(3 << 30),
        time_seconds: Some(600),
        ..Placement::default()
    };
    let job = Job {
        name,
        spec,
        staging,
        placement: &placement,
    };
    backend.run(job, &CancellationToken::new()).await
}

/// A task runs as a Flyte run of a raw container: Flyte downloads the helper, which stages the
/// input in and the output back. A refused token is renewed once, and the exit code of a
/// failed task comes from the wrapper.
#[tokio::test]
async fn runs_flyte_tasks() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeFlyte::start(&folder.path().join("fake"));
    let backend = backend(folder.path(), &fake, None);
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
    let upper = spec("tr a-z A-Z < in.txt > out.txt");
    let finished = run(&backend, "run-UPPER-1-1", &upper, &staging)
        .await
        .expect("the run ends");
    assert_eq!(finished.exit_code, Some(0));
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("the output");
    assert_eq!(output, "HELLO\n");
    let created = fake.requests("/flyteidl2.workflow.RunService/CreateRun");
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].token, "Bearer token-1");
    let body = created[0].json();
    let id = &body["runId"];
    assert_eq!(
        (&id["org"], &id["project"], &id["domain"]),
        (&"lab".into(), &"flows".into(), &"development".into())
    );
    let template = &body["taskSpec"]["taskTemplate"];
    assert_eq!(template["type"], "raw-container");
    assert_eq!(template["container"]["image"], IMAGE);
    assert_eq!(template["metadata"]["timeout"], "600s");
    assert_eq!(
        template["container"]["resources"]["limits"][1]["value"],
        "3072Mi"
    );
    let uri = &body["inputs"]["literals"][0]["value"]["scalar"]["blob"]["uri"];
    assert!(
        uri.as_str().is_some_and(|uri| uri.ends_with("/helper")),
        "{uri}"
    );
    assert_eq!(fake.requests("/oauth2/token").len(), 1);
    fake.rotate();
    let failing = spec("exit 3");
    let failed = run(&backend, "run-FAIL-1-1", &failing, &Staging::default()).await;
    assert_eq!(failed.expect("the run ends").exit_code, Some(3));
    assert_eq!(
        fake.requests("/oauth2/token").len(),
        2,
        "a refused token is renewed"
    );
}

/// A run that cannot start the helper fails as a failure of the machine, with the end of its
/// log from the log stream.
#[tokio::test]
async fn reports_lost_runs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeFlyte::start(&folder.path().join("fake"));
    let wrong = "0".repeat(64);
    let backend = backend(folder.path(), &fake, Some(&wrong));
    let lost = run(&backend, "run-LOST-1-1", &spec("true"), &Staging::default()).await;
    let Err(ExecutorError::Engine { message, .. }) = lost else {
        panic!("a failure of the machine: {lost:?}");
    };
    assert!(message.contains("exit code 125"), "{message}");
    assert!(message.contains("does not match its SHA-256"), "{message}");
    assert_eq!(
        fake.requests("/flyteidl2.workflow.RunLogsService/TailLogs")
            .len(),
        1
    );
}

/// A cancelled task aborts its run, which ends its container.
#[tokio::test]
async fn cancels_running_runs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeFlyte::start(&folder.path().join("fake"));
    let backend = backend(folder.path(), &fake, None);
    let spec = spec("sleep 60");
    let (placement, staging) = (Placement::default(), Staging::default());
    let job = Job {
        name: "run-SLOW-1-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let cancel = CancellationToken::new();
    let stop = async {
        for _ in 0..3000 {
            if fake.runs().first().is_some_and(|name| fake.running(name)) {
                cancel.cancel();
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the run never started");
    };
    let (ended, ()) = tokio::join!(backend.run(job, &cancel), stop);
    assert!(matches!(ended, Err(ExecutorError::Cancelled)), "{ended:?}");
    let aborted = fake.requests("/flyteidl2.workflow.RunService/AbortRun");
    assert_eq!(aborted.len(), 1);
    let name = fake.runs()[0].clone();
    assert_eq!(aborted[0].json()["runId"]["name"], name.as_str());
    // The fake notes the end of the killed command when its thread has waited for it.
    for _ in 0..3000 {
        if !fake.running(&name) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!fake.running(&name));
}
