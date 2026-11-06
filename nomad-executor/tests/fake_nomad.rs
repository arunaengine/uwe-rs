//! Checks the Nomad backend against a local fake of the Nomad HTTP API that runs each job's
//! command here, and its job bodies against the requests of the real nf-nomad.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use executor::remote::RemoteStage;
use executor::{Backend, ExecutorError, Job, Staging};
use nomad_executor::{Cleanup, Client, NomadBackend, Settings};
use task_model::{FORMAT, Image, OutputRule, Placement, TaskSpec};
use tokio_util::sync::CancellationToken;

use common::FakeNomad;

/// An image with a digest, which needs no registry.
const IMAGE: &str =
    "ubuntu@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

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

fn spec_small() -> TaskSpec {
    let mut small = spec("true");
    small.outputs.clear();
    small
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
