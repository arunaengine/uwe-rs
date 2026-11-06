//! Checks the Snowflake backend against a local fake of the Snowflake SQL API that runs the
//! containers of each job service here: key-pair and token sign-in, helper images, logs, cancel.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use executor::batch::Helper;
use executor::remote::RemoteStage;
use executor::{Backend, ExecutorError, Job, Staged, Staging};
use snowflake_executor::{Auth, Client, Context, KeyPair, Settings, SnowflakeBackend};
use task_model::{FORMAT, Image, OutputRule, Placement, TaskSpec};
use tokio_util::sync::CancellationToken;

use common::{FakeSnowflake, TOKEN};

/// An image with a digest, which needs no registry.
const IMAGE: &str = "docker.io/library/ubuntu@sha256:\
                     0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The test program as the helper of the jobs: it runs `stage_helper` with the arguments.
fn helper(folder: &Path) -> PathBuf {
    let program = std::env::current_exe().expect("this program");
    let script = format!(
        "#!/bin/sh\nIFS=$(printf '\\037')\nUWE_TEST_STAGE=\"$*\" exec '{}' --exact \
         stage_helper --nocapture --test-threads=1\n",
        program.to_string_lossy().replace('\'', r"'\''")
    );
    executable(&folder.join("helper"), &script)
}

/// A program that stands for `/uwe __stage install <file>` of a helper image: it copies the
/// test helper to the file.
fn installer(folder: &Path) -> PathBuf {
    let helper = helper(folder);
    let script = format!("#!/bin/sh\ncp '{}' \"$3\"\n", helper.display());
    executable(&folder.join("installer"), &script)
}

fn executable(path: &Path, script: &str) -> PathBuf {
    std::fs::create_dir_all(path.parent().expect("a folder")).expect("makes the folder");
    std::fs::write(path, script).expect("writes the program");
    let mode = std::os::unix::fs::PermissionsExt::from_mode(0o755);
    std::fs::set_permissions(path, mode).expect("runs");
    path.to_owned()
}

#[test]
fn stage_helper() {
    if let Ok(arguments) = std::env::var("UWE_TEST_STAGE") {
        let arguments: Vec<String> = arguments.split('\u{1f}').map(str::to_owned).collect();
        // The first argument is `__stage`, as the uwe program takes it.
        executor::helper::main(&arguments[1..]).expect("the helper stages");
    }
}

/// Settings with the compute pool `TASKS`, the integration `STAGING`, a registry mapping and
/// attempt folders in `folder`.
fn settings(folder: &Path) -> Settings {
    Settings {
        compute_pool: Some("TASKS".into()),
        integrations: vec!["STAGING".into()],
        mappings: vec![("docker.io".into(), "/lab/images/repo".into())],
        poll: Duration::from_millis(50),
        task_folder: folder.join("node").to_string_lossy().into_owned(),
        ..Settings::default()
    }
}

/// A backend whose calls go to the fake with `auth`, through a staging root in `folder`.
fn backend(
    folder: &Path,
    fake: &FakeSnowflake,
    auth: Auth,
    settings: Settings,
) -> SnowflakeBackend {
    let context = Context {
        database: Some("LAB".into()),
        schema: Some("JOBS".into()),
        ..Context::default()
    };
    let mut client = Client::new(&fake.address, auth, context);
    client.pause = Duration::from_millis(10);
    let stage = RemoteStage {
        objects: content_store::Store::local(&folder.join("staging")).expect("a store"),
        expiry: Duration::from_secs(600),
        max_outputs: 4,
        credentials: false,
        cleanup: true,
    };
    let scratch = folder.join("scratch");
    let mut backend =
        SnowflakeBackend::new(client, settings, scratch, stage, None).expect("a backend");
    backend.staging_time = Duration::ZERO;
    backend
}

fn key_pair() -> Auth {
    let pem = include_str!("fixtures/snowflake_key.pem");
    Auth::KeyPair(Box::new(
        KeyPair::new("myorg-lab", "uwe", pem).expect("a key"),
    ))
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
    backend: &SnowflakeBackend,
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

/// A task runs as a job service with a key-pair token: the helper image places the helper,
/// which stages the input in and the output back; the job service is dropped at its end, and
/// a failed task keeps its exit code.
#[tokio::test]
async fn runs_snowflake_jobs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeSnowflake::start(&folder.path().join("fake"), Some(installer(folder.path())));
    let settings = Settings {
        helper_image: Some("/lab/images/repo/uwe:0.1.0".into()),
        ..settings(folder.path())
    };
    let backend = backend(folder.path(), &fake, key_pair(), settings);
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
        .expect("the job ends");
    assert_eq!(finished.exit_code, Some(0));
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("the output");
    assert_eq!(output, "HELLO\n");
    let submitted = fake.requests("POST /api/v2/statements?async=true&requestId=");
    assert!(
        submitted
            .iter()
            .all(|request| request.kind == "KEYPAIR_JWT")
    );
    let body = submitted[0].json();
    assert_eq!(
        (&body["database"], &body["schema"], &body["timeout"]),
        (&"LAB".into(), &"JOBS".into(), &600.into())
    );
    let started = fake.statements("EXECUTE JOB SERVICE");
    assert_eq!(started.len(), 1);
    let text = &started[0];
    assert!(text.contains("\nIN COMPUTE POOL TASKS\n"), "{text}");
    assert!(
        text.contains("\nEXTERNAL_ACCESS_INTEGRATIONS = (STAGING)\n"),
        "{text}"
    );
    assert!(
        text.contains("\"image\":\"/lab/images/repo/library/ubuntu@sha256:"),
        "{text}"
    );
    assert!(
        text.contains("\"image\":\"/lab/images/repo/uwe:0.1.0\""),
        "{text}"
    );
    assert!(!text.contains("privileged"), "{text}");
    assert_eq!(fake.statements("DROP SERVICE IF EXISTS UWE_").len(), 1);
    let failing = spec("exit 3");
    let failed = run(&backend, "run-FAIL-1-1", &failing, &Staging::default()).await;
    assert_eq!(failed.expect("the job ends").exit_code, Some(3));
    assert!(fake.statements("SELECT SYSTEM$GET_SERVICE_LOGS").is_empty());
}

/// A job that cannot start the helper fails as a failure of the machine, with the end of the
/// log of its main container; the access token signs the calls.
#[tokio::test]
async fn reports_lost_jobs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeSnowflake::start(&folder.path().join("fake"), None);
    let missing = folder
        .path()
        .join("no-helper")
        .to_string_lossy()
        .into_owned();
    let settings = Settings {
        helper: Some(Helper::Path(missing)),
        ..settings(folder.path())
    };
    let auth = Auth::Token {
        kind: snowflake_executor::client::ACCESS_TOKEN,
        token: TOKEN.into(),
    };
    let backend = backend(folder.path(), &fake, auth, settings);
    let lost = run(&backend, "run-LOST-1-1", &spec("true"), &Staging::default()).await;
    let Err(ExecutorError::Engine { message, .. }) = lost else {
        panic!("a failure of the machine: {lost:?}");
    };
    assert!(message.contains("could not run the task"), "{message}");
    assert!(message.contains("no-helper"), "{message}");
    let submitted = fake.requests("POST /api/v2/statements?");
    assert!(
        submitted
            .iter()
            .all(|request| request.kind == "PROGRAMMATIC_ACCESS_TOKEN")
    );
    assert_eq!(
        fake.statements("SELECT SYSTEM$GET_SERVICE_LOGS('UWE_")
            .len(),
        1
    );
}

/// A cancelled task cancels the statement of its job, which ends its container, and drops it.
#[tokio::test]
async fn cancels_running_jobs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeSnowflake::start(&folder.path().join("fake"), None);
    let settings = Settings {
        helper: Some(Helper::Path(
            helper(folder.path()).to_string_lossy().into_owned(),
        )),
        ..settings(folder.path())
    };
    let backend = backend(folder.path(), &fake, key_pair(), settings);
    let slow = spec("sleep 60");
    let (placement, staging) = (Placement::default(), Staging::default());
    let job = Job {
        name: "run-SLOW-1-1",
        spec: &slow,
        staging: &staging,
        placement: &placement,
    };
    let cancel = CancellationToken::new();
    let stop = async {
        for _ in 0..3000 {
            if fake.running() {
                cancel.cancel();
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the job never started");
    };
    let (ended, ()) = tokio::join!(backend.run(job, &cancel), stop);
    assert!(matches!(ended, Err(ExecutorError::Cancelled)), "{ended:?}");
    assert_eq!(
        fake.requests("POST /api/v2/statements/01b0-1/cancel").len(),
        1
    );
    assert_eq!(fake.statements("DROP SERVICE IF EXISTS UWE_").len(), 1);
    assert!(!fake.running());
}
