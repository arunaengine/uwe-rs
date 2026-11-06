//! Checks the AWS Batch backend against a local fake of AWS Batch and CloudWatch Logs that runs
//! each job's command here.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use aws_batch_executor::{BatchBackend, Client, Helper, Settings, Volume};
use executor::remote::RemoteStage;
use executor::{Backend, Job, Staging};
use serde_json::Value;
use storage::aws::{Credentials, Keys};
use task_model::{Accelerator, FORMAT, Image, OutputRule, Placement, TaskSpec};
use tokio_util::sync::CancellationToken;

use common::FakeBatch;

/// An image with a digest, which needs no registry.
const IMAGE: &str =
    "ubuntu@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The test program as the helper of the jobs: it runs `stage_helper` with the arguments.
fn helper(folder: &Path) -> String {
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
    path.to_string_lossy().into_owned()
}

#[test]
fn stage_helper() {
    if let Ok(arguments) = std::env::var("UWE_TEST_STAGE") {
        let arguments: Vec<String> = arguments.split('\u{1f}').map(str::to_owned).collect();
        // The first argument is `__stage`, as the uwe program takes it.
        executor::helper::main(&arguments[1..]).expect("the helper stages");
    }
}

/// A backend in `folder` whose client calls the fake, with the settings of the nf-amazon
/// fixture.
fn backend(folder: &Path, fake: &FakeBatch) -> BatchBackend {
    let credentials = Credentials::Keys(Keys {
        id: "AKIDEXAMPLE".into(),
        secret: "secret".into(),
        token: None,
    });
    let mut client = Client::new("eu-west-1", credentials, Duration::from_millis(1));
    (client.batch, client.logs) = (fake.address.clone(), fake.address.clone());
    let settings = Settings {
        region: Some("eu-west-1".into()),
        job_role: Some("arn:aws:iam::000000000000:role/probe-job".into()),
        spot_attempts: 3,
        volumes: ["/scratch", "/refs:/refs:ro"]
            .map(|text| Volume::parse(text).expect("a volume"))
            .to_vec(),
        logs_group: Some("/probe/batch".into()),
        share: Some("blue".into()),
        priority: 10,
        poll: Duration::from_millis(50),
        task_folder: folder.join("node").to_string_lossy().into_owned(),
        helper: Some(Helper::Path(helper(folder))),
        ..Settings::default()
    };
    let stage = RemoteStage {
        objects: content_store::Store::local(&folder.join("staging")).expect("a store"),
        expiry: Duration::from_secs(600),
        max_outputs: 4,
        credentials: false,
        cleanup: true,
    };
    let mut backend = BatchBackend::new(client, settings, folder.join("scratch"), stage, None)
        .expect("a backend");
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

/// The resources and labels of the `ALIGN` process of the fixture.
fn placement() -> Placement {
    Placement {
        cpus: 2,
        memory_bytes: Some(3 << 30),
        time_seconds: Some(3600),
        accelerators: vec![Accelerator {
            count: 1,
            kind: None,
        }],
        queue: Some("probe-queue".into()),
        labels: [("team", "genomics"), ("cost-center", "A 12")]
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .into(),
        ..Placement::default()
    }
}

/// A second run finds the definition of the first one by its digest instead of registering
/// another.
#[tokio::test]
async fn reuses_job_definitions() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeBatch::start(&folder.path().join("fake"));
    let spec = spec("echo hi > out.txt");
    let placement = placement();
    let staging = Staging::default();
    for (number, name) in ["run-A-1-1", "run-B-1-1"].iter().enumerate() {
        let mut backend = backend(&folder.path().join(format!("run{number}")), &fake);
        // The folder of the helper is part of the definition.
        backend.settings.helper = Some(Helper::Path(helper(folder.path())));
        let job = Job {
            name,
            spec: &spec,
            staging: &staging,
            placement: &placement,
        };
        let finished = backend
            .run(job, &CancellationToken::new())
            .await
            .expect("the job ends");
        assert_eq!(finished.exit_code, Some(0));
    }
    assert_eq!(fake.requests("registerjobdefinition").len(), 1);
    assert_eq!(fake.requests("describejobdefinitions").len(), 2);
    let definitions: Vec<Value> = fake
        .requests("submitjob")
        .iter()
        .map(|request| request.body["jobDefinition"].clone())
        .collect();
    assert_eq!(definitions[0], definitions[1]);
}
