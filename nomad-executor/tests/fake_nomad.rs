//! Checks the Nomad backend against a local fake of the Nomad HTTP API that runs each job's
//! command here, and its job bodies against the requests of the real nf-nomad.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use executor::batch::Helper;
use executor::remote::RemoteStage;
use executor::{Backend, ExecutorError, Job, Staged, Staging};
use nomad_executor::{Cleanup, Client, NomadBackend, Settings};
use serde_json::{Value, json};
use task_model::{Accelerator, FORMAT, Image, OutputRule, Placement, TaskSpec};
use tokio_util::sync::CancellationToken;

use common::FakeNomad;

/// An image with a digest, which needs no registry.
const IMAGE: &str =
    "ubuntu@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The test program as the helper of the jobs: it runs `stage_helper` with the arguments.
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
    let mode = std::os::unix::fs::PermissionsExt::from_mode(0o755);
    std::fs::set_permissions(&path, mode).expect("runs");
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

/// Settings like those of the fixture: one datacenter, the namespace `lab`, no cleanup.
fn settings(fake: &FakeNomad) -> Settings {
    Settings {
        address: Some(fake.address.clone()),
        namespace: Some("lab".into()),
        datacenters: vec!["dc1".into()],
        network_mode: Some("bridge".into()),
        cleanup: Cleanup::Never,
        poll: Duration::from_millis(50),
        ..Settings::default()
    }
}

/// A backend whose calls go to the fake; with `stage`, through a staging root in `folder`.
fn backend(folder: &Path, settings: Settings, stage: bool) -> NomadBackend {
    let address = settings.address.clone().unwrap_or_default();
    let mut client = Client::new(
        &address,
        Some("not-a-real-token".into()),
        Some("lab".into()),
        None,
    );
    client.pause = Duration::from_millis(10);
    let stage = stage.then(|| RemoteStage {
        objects: content_store::Store::local(&folder.join("staging")).expect("a store"),
        expiry: Duration::from_secs(600),
        max_outputs: 4,
        credentials: false,
        cleanup: true,
    });
    let scratch = folder.join("scratch");
    let mut backend = NomadBackend::new(client, settings, scratch, stage, None).expect("a backend");
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

/// The resources and `nomadOptions` of the `ALIGN` process of the fixture.
fn align() -> Placement {
    let options = json!({
        "nodePool": "highmem",
        "meta": {"owner": "team-x"},
        "priority": "high",
        "failures": {"reschedule": {"attempts": 2, "delay": "10s"}},
    });
    Placement {
        cpus: 2,
        memory_bytes: Some(3 << 30),
        time_seconds: Some(3600),
        accelerators: vec![Accelerator {
            count: 1,
            kind: None,
        }],
        options: [("nomadOptions".to_owned(), options)].into(),
        ..Placement::default()
    }
}

/// The body of a recorded job registration of the fixture, by its process.
fn recorded(process: &str) -> Value {
    let text = include_str!("fixtures/nf_nomad_requests.jsonl");
    let request = (text.lines())
        .map(|line| serde_json::from_str::<Value>(line).expect("a JSON line"))
        .find(|request| {
            request["method"] == "POST"
                && request["body"]["Job"]["Meta"]["nf_process_name"] == process
        })
        .expect("a recorded registration");
    assert_eq!(request["path"], "/v1/jobs?namespace=lab");
    request["body"]["Job"].clone()
}

async fn run(
    backend: &NomadBackend,
    name: &str,
    spec: &TaskSpec,
    placement: &Placement,
    staging: &Staging,
) -> Result<executor::Finished, ExecutorError> {
    let job = Job {
        name,
        spec,
        staging,
        placement,
    };
    backend.run(job, &CancellationToken::new()).await
}

/// Two tasks of shared folders run as the fixture's `ALIGN` and `SMALL`: their jobs have the
/// fields that nf-nomad sends, but no privileges, and Nomad's durations in nanoseconds.
#[tokio::test]
async fn matches_recorded_jobs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeNomad::start(&folder.path().join("fake"));
    let backend = backend(folder.path(), settings(&fake), false);
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
    let spec = spec("tr a-z A-Z < in.txt > out.txt");
    let finished = run(&backend, "run-ALIGN-1-1", &spec, &align(), &staging)
        .await
        .expect("the job ends");
    assert_eq!(finished.exit_code, Some(0));
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("the output");
    assert_eq!(output, "HELLO\n");
    let small = Placement::default();
    let empty = Staging::default();
    let finished = run(&backend, "run-SMALL-2-1", &spec_small(), &small, &empty).await;
    assert_eq!(finished.expect("the job ends").exit_code, Some(0));
    let registered = fake.requests("POST /v1/jobs");
    assert_eq!(registered.len(), 2);
    assert!(
        registered
            .iter()
            .all(|request| request.path == "/v1/jobs?namespace=lab")
    );
    assert!(
        registered
            .iter()
            .all(|request| request.token == "not-a-real-token")
    );
    for (request, process) in registered.iter().zip(["ALIGN", "SMALL"]) {
        let ours = &request.json()["Job"];
        let theirs = recorded(process);
        for key in ["Type", "Datacenters", "Namespace", "NodePool", "Priority"] {
            assert_eq!(ours[key], theirs[key], "{process} {key}");
        }
        let (group, recorded_group) = (&ours["TaskGroups"][0], &theirs["TaskGroups"][0]);
        assert_eq!(
            group["RestartPolicy"]["Attempts"],
            recorded_group["RestartPolicy"]["Attempts"]
        );
        let reschedule = &group["ReschedulePolicy"];
        assert_eq!(
            reschedule["Attempts"],
            recorded_group["ReschedulePolicy"]["Attempts"]
        );
        let (task, recorded_task) = (&group["Tasks"][0], &recorded_group["Tasks"][0]);
        assert_eq!(task["Driver"], recorded_task["Driver"]);
        assert_eq!(task["Resources"], recorded_task["Resources"], "{process}");
        assert_eq!(
            task["Config"]["network_mode"],
            recorded_task["Config"]["network_mode"]
        );
        // nf-nomad starts privileged containers; uwe never does.
        assert_eq!(recorded_task["Config"]["privileged"], true);
        assert_eq!(task["Config"]["privileged"], false);
        assert_eq!(task["Config"]["image"], IMAGE);
        let owner = &theirs["Meta"]["owner"];
        assert_eq!(&ours["Meta"]["owner"], owner, "{process}");
    }
    // nf-nomad sends the delay of `10s` as 10000, which Nomad reads as nanoseconds.
    let align = &registered[0].json()["Job"]["TaskGroups"][0]["ReschedulePolicy"];
    assert_eq!(
        recorded("ALIGN")["TaskGroups"][0]["ReschedulePolicy"]["Delay"],
        10_000
    );
    assert_eq!(align["Delay"], 10_000_000_000u64);
    let task = &registered[0].json()["Job"]["TaskGroups"][0]["Tasks"][0];
    let user = task["User"].as_str().expect("a user");
    assert!(user.contains(':') && !user.starts_with("0:"), "{user}");
    assert!(!fake.requests("GET /v1/job/").is_empty());
    assert!(
        fake.requests("DELETE").is_empty(),
        "cleanup never keeps the jobs"
    );
}

fn spec_small() -> TaskSpec {
    let mut small = spec("true");
    small.outputs.clear();
    small
}

/// A task of a staging root gets the helper by an artifact with its SHA-256, which the job
/// command places, and its inputs and outputs through the staging root.
#[tokio::test]
async fn runs_staged_jobs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeNomad::start(&folder.path().join("fake"));
    let helper = helper(folder.path());
    let bytes = std::fs::read(&helper).expect("reads the helper");
    let sha256 = task_model::Digest::sha256(&bytes).hex().to_owned();
    let settings = Settings {
        task_folder: folder.path().join("node").to_string_lossy().into_owned(),
        helper: Some(Helper::Download {
            url: format!("file://{}", helper.display()),
            sha256: sha256.clone(),
        }),
        cleanup: Cleanup::Always,
        ..settings(&fake)
    };
    let backend = backend(folder.path(), settings, true);
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
    let spec = spec("tr a-z A-Z < in.txt > out.txt");
    let finished = run(
        &backend,
        "run-UPPER-1-1",
        &spec,
        &Placement::default(),
        &staging,
    )
    .await
    .expect("the job ends");
    assert_eq!(finished.exit_code, Some(0));
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("the output");
    assert_eq!(output, "HELLO\n");
    let body = fake.requests("POST /v1/jobs")[0].json();
    let task = &body["Job"]["TaskGroups"][0]["Tasks"][0];
    let artifact = &task["Artifacts"][0];
    assert_eq!(
        artifact["GetterOptions"]["checksum"],
        format!("sha256:{sha256}")
    );
    assert_eq!(artifact["RelativeDest"], "local/uwe");
    assert!(task.get("User").is_none(), "{task}");
    let id = &fake.jobs()[0];
    assert!(fake.purged(id), "cleanup always purges the job");
}

/// A cancelled task stops its job, which ends its command.
#[tokio::test]
async fn cancels_running_jobs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeNomad::start(&folder.path().join("fake"));
    let backend = backend(folder.path(), settings(&fake), false);
    let spec = spec("sleep 60");
    let placement = Placement::default();
    let staging = Staging::default();
    let job = Job {
        name: "run-SLOW-1-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let cancel = CancellationToken::new();
    let stop = async {
        for _ in 0..3000 {
            if fake.jobs().first().is_some_and(|id| fake.running(id)) {
                cancel.cancel();
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the job never ran");
    };
    let (ended, ()) = tokio::join!(backend.run(job, &cancel), stop);
    assert!(matches!(ended, Err(ExecutorError::Cancelled)), "{ended:?}");
    let id = fake.jobs()[0].clone();
    let stopped = fake.requests("DELETE ");
    assert_eq!(stopped.len(), 1);
    assert!(
        stopped[0].path.starts_with(&format!("/v1/job/{id}?")),
        "{}",
        stopped[0].path
    );
    assert!(!fake.running(&id));
}

/// A job that no node fits fails at once with `fail_unplaced`, with the reason of Nomad.
#[tokio::test]
async fn fails_unplaceable_jobs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeNomad::start(&folder.path().join("fake"));
    let settings = Settings {
        fail_unplaced: true,
        ..settings(&fake)
    };
    let backend = backend(folder.path(), settings, false);
    fake.unplaceable();
    let (placement, staging) = (Placement::default(), Staging::default());
    let unplaced = run(
        &backend,
        "run-NOWHERE-1-1",
        &spec_small(),
        &placement,
        &staging,
    )
    .await;
    let Err(ExecutorError::Unsupported(message)) = unplaced else {
        panic!("a placement failure: {unplaced:?}");
    };
    assert!(message.contains("no suitable nodes"), "{message}");
    assert!(message.contains("2 of 2 nodes"), "{message}");
    assert_eq!(
        fake.requests("GET /v1/job/uwe-run-nowhere-1-1/evaluations")
            .len(),
        1
    );
    let id = &fake.jobs()[0];
    assert!(fake.purged(id), "a job that cannot run is purged");
}
