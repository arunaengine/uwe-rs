//! Checks the AWS Batch backend against a local fake of AWS Batch and CloudWatch Logs that runs
//! each job's command here, and compares its requests with those of nf-amazon.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use aws_batch_executor::{BatchBackend, Client, Settings, Volume};
use executor::batch::Helper;
use executor::remote::RemoteStage;
use executor::{Backend, ExecutorError, Job, Staged, Staging};
use serde_json::{Value, json};
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

/// The body of the recorded request to `path` that `pick` selects.
fn recorded(path: &str, pick: impl Fn(&Value) -> bool) -> Value {
    let text = include_str!("fixtures/nf_amazon_requests.jsonl");
    let requests = text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("JSON"));
    let mut found = requests
        .filter(|request| request["path"] == path)
        .map(|request| request["body"].clone());
    found.find(|body| pick(body)).expect("a recorded request")
}

/// Runs a task with an input through the fake, and compares the requests with those that
/// nf-amazon sent for the same settings.
#[tokio::test]
async fn runs_staged_jobs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeBatch::start(&folder.path().join("fake"));
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
    let spec = spec("tr a-z A-Z < in.txt > out.txt");
    let placement = placement();
    let job = Job {
        name: "run-ALIGN-1-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let finished = backend
        .run(job, &CancellationToken::new())
        .await
        .expect("the job ends");
    assert_eq!(finished.exit_code, Some(0));
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("the output");
    assert_eq!(output, "HELLO\n");
    let submitted = &fake.requests("submitjob")[0];
    let signed = "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/";
    assert!(
        submitted.authorization.starts_with(signed),
        "{}",
        submitted.authorization
    );
    assert!(
        submitted
            .authorization
            .contains("/eu-west-1/batch/aws4_request")
    );
    let body = &submitted.body;
    let native = recorded("/v1/submitjob", |body| body["jobName"] == "ALIGN");
    for field in [
        "jobQueue",
        "retryStrategy",
        "schedulingPriorityOverride",
        "shareIdentifier",
        "tags",
        "propagateTags",
        "timeout",
    ] {
        assert_eq!(body[field], native[field], "{field}");
    }
    let resources = "resourceRequirements";
    assert_eq!(
        body["containerOverrides"][resources],
        native["containerOverrides"][resources]
    );
    assert_eq!(body["jobName"], "run-ALIGN-1-1");
    assert_eq!(
        body["jobDefinition"],
        "uwe-ubuntu-sha256-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef:1"
    );
    let registered = &fake.requests("registerjobdefinition")[0].body;
    let native = recorded("/v1/registerjobdefinition", |body| {
        body["jobDefinitionName"] == "nf-ubuntu-24-04"
    });
    let (ours, theirs) = (
        &registered["containerProperties"],
        &native["containerProperties"],
    );
    for field in [
        "command",
        "jobRoleArn",
        "logConfiguration",
        "resourceRequirements",
    ] {
        assert_eq!(ours[field], theirs[field], "{field}");
    }
    // The fixture's two volumes, then the folder of the helper on the host.
    for field in ["volumes", "mountPoints"] {
        assert_eq!(
            ours[field].as_array().expect("a list")[..2],
            theirs[field].as_array().expect("a list")[..],
            "{field}"
        );
    }
    assert_eq!(ours["mountPoints"][2]["readOnly"], true);
    assert_eq!(registered["type"], "container");
    assert!(
        registered["parameters"]["uwe-digest"]
            .as_str()
            .is_some_and(|digest| digest.len() == 64)
    );
    let described = &fake.requests("describejobdefinitions")[0].body;
    assert_eq!(described["status"], "ACTIVE");
    assert_eq!(
        fake.requests("describejobs")[0].body,
        json!({"jobs": ["job-1"]})
    );
    // The objects of the attempt went away with its collection.
    assert!(
        !folder
            .path()
            .join("staging/attempts")
            .read_dir()
            .is_ok_and(|mut entries| entries.next().is_some())
    );
}

/// Stages a task through an S3 bucket with presigned requests, as on AWS, while the fake runs
/// the job here.
#[tokio::test]
#[ignore = "needs an S3 bucket; set UWE_S3_BUCKET, UWE_S3_ENDPOINT and AWS credentials, or run \
            `just s3-test`"]
async fn stages_through_s3() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeBatch::start(&folder.path().join("fake"));
    let mut backend = backend(folder.path(), &fake);
    let options = content_store::S3Options {
        bucket: std::env::var("UWE_S3_BUCKET").expect("UWE_S3_BUCKET is set"),
        prefix: Some(format!("uwe-batch-{}", std::process::id())),
        endpoint: std::env::var("UWE_S3_ENDPOINT").ok(),
        region: Some(std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".into())),
    };
    backend.stage.objects = content_store::Store::s3(&options).expect("a store");
    let source = folder.path().join("in.txt");
    std::fs::write(&source, "hello s3\n").expect("writes the input");
    let staging = Staging {
        inputs: vec![Staged {
            path: "in.txt".into(),
            source,
            ..Staged::default()
        }],
        ..Staging::default()
    };
    let spec = spec("tr a-z A-Z < in.txt > out.txt");
    let placement = placement();
    let job = Job {
        name: "run-S3-1-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let finished = backend
        .run(job, &CancellationToken::new())
        .await
        .expect("the job ends");
    assert_eq!(finished.exit_code, Some(0));
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("the output");
    assert_eq!(output, "HELLO S3\n");
    let command = &fake.requests("submitjob")[0].body["containerOverrides"]["command"];
    let plan = command[5].as_str().expect("the plan");
    assert!(
        plan.starts_with("http") && plan.contains("X-Amz-Signature"),
        "{plan}"
    );
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

/// A cancelled task stops its job, which then ends.
#[tokio::test]
async fn cancels_running_jobs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeBatch::start(&folder.path().join("fake"));
    let backend = backend(folder.path(), &fake);
    let spec = spec("sleep 60");
    let placement = placement();
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
            if fake.running("job-1") {
                cancel.cancel();
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the job never ran");
    };
    let (ended, ()) = tokio::join!(backend.run(job, &cancel), stop);
    assert!(matches!(ended, Err(ExecutorError::Cancelled)), "{ended:?}");
    let stopped = fake.requests("terminatejob");
    assert_eq!(
        stopped[0].body,
        json!({"jobId": "job-1", "reason": "uwe cancelled the task"})
    );
    assert!(!fake.running("job-1"));
}

/// A job that cannot even stage fails with the end of its log, and a throttled submission is
/// sent again.
#[tokio::test]
async fn reports_job_logs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeBatch::start(&folder.path().join("fake"));
    fake.throttle(2);
    let mut backend = backend(folder.path(), &fake);
    backend.settings.helper = Some(Helper::Path("/nonexistent/uwe".into()));
    let spec = spec("true");
    let placement = placement();
    let staging = Staging::default();
    let job = Job {
        name: "run-BROKEN-1-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let ended = backend.run(job, &CancellationToken::new()).await;
    let Err(ExecutorError::Engine { engine, message }) = ended else {
        panic!("a failure of the machine: {ended:?}");
    };
    assert_eq!(engine, "AWS Batch");
    assert!(message.contains("exit code 125"), "{message}");
    assert!(message.contains("/nonexistent/uwe"), "{message}");
    assert_eq!(fake.requests("submitjob").len(), 3);
    let read = &fake.requests("GetLogEvents")[0].body;
    assert_eq!(read["logGroupName"], "/probe/batch");
    assert_eq!(read["logStreamName"], "uwe/default/job-1");
}

/// A job that no compute environment can run is stopped when the settings say so, and a task
/// without a queue is refused before anything is sent.
#[tokio::test]
async fn stops_unschedulable_jobs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeBatch::start(&folder.path().join("fake"));
    fake.unschedulable();
    let mut backend = backend(folder.path(), &fake);
    backend.settings.terminate_unschedulable = true;
    let spec = spec("true");
    let staging = Staging::default();
    let placement = placement();
    let job = Job {
        name: "run-BIG-1-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let ended = backend.run(job, &CancellationToken::new()).await;
    let Err(ExecutorError::Unsupported(message)) = ended else {
        panic!("a permanent failure: {ended:?}");
    };
    assert!(
        message.contains("MISCONFIGURATION:JOB_RESOURCE_REQUIREMENT"),
        "{message}"
    );
    assert_eq!(fake.requests("terminatejob").len(), 1);
    let unqueued = Placement {
        queue: None,
        ..placement.clone()
    };
    let job = Job {
        name: "run-NOQUEUE-1-1",
        placement: &unqueued,
        ..job
    };
    let ended = backend.run(job, &CancellationToken::new()).await;
    assert!(
        matches!(&ended, Err(ExecutorError::Unsupported(message)) if message.contains("job queue")),
        "{ended:?}"
    );
    assert_eq!(fake.requests("submitjob").len(), 1);
}
