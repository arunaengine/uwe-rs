//! Checks the Google Batch backend against a local fake of Google Batch, the OAuth token
//! endpoint and Cloud Logging that runs each job's runnables here.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use executor::batch::Helper;
use executor::remote::RemoteStage;
use executor::{Backend, ExecutorError, Job, Staged, Staging};
use google_batch_executor::{Client, Credentials, GoogleBackend, Settings};
use rsa::RsaPrivateKey;
use rsa::pkcs8::{EncodePrivateKey, LineEnding};
use rsa::rand_core::{CryptoRng, RngCore};
use rsa::signature::Verifier;
use serde_json::{Value, json};
use task_model::{FORMAT, Image, OutputRule, Placement, TaskSpec};
use tokio_util::sync::CancellationToken;

use common::FakeGoogle;

/// An image with a digest, which needs no registry.
const IMAGE: &str =
    "ubuntu@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A generator of fixed bytes, so that every run makes the same test key.
struct Seeded(u64);

impl RngCore for Seeded {
    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }

    fn next_u64(&mut self) -> u64 {
        // SplitMix64.
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    fn fill_bytes(&mut self, bytes: &mut [u8]) {
        for chunk in bytes.chunks_mut(8) {
            let value = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&value[..chunk.len()]);
        }
    }

    fn try_fill_bytes(&mut self, bytes: &mut [u8]) -> Result<(), rsa::rand_core::Error> {
        self.fill_bytes(bytes);
        Ok(())
    }
}

impl CryptoRng for Seeded {}

/// A made-up service account key, the same in every run.
fn key() -> &'static RsaPrivateKey {
    static KEY: OnceLock<RsaPrivateKey> = OnceLock::new();
    KEY.get_or_init(|| RsaPrivateKey::new(&mut Seeded(7), 1024).expect("a key"))
}

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

/// A backend in `folder` whose calls go to the fake, with a service account key file whose
/// `token_uri` is the fake.
fn backend(folder: &Path, fake: &FakeGoogle) -> GoogleBackend {
    let pem = key().to_pkcs8_pem(LineEnding::LF).expect("a PEM key");
    let account = json!({
        "type": "service_account",
        "project_id": "lab",
        "private_key_id": "key-1",
        "private_key": pem.as_str(),
        "client_email": "uwe@lab.iam.gserviceaccount.com",
        "token_uri": format!("{}/token", fake.address),
    });
    std::fs::create_dir_all(folder).expect("makes the folder");
    let file = folder.join("account.json");
    std::fs::write(&file, account.to_string()).expect("writes the key");
    let credentials = Credentials::read(&file).expect("credentials");
    let settings = Settings {
        location: Some("europe-west4".into()),
        spot: true,
        spot_attempts: 2,
        service_account: Some("jobs@lab.iam.gserviceaccount.com".into()),
        poll: Duration::from_millis(50),
        task_folder: folder.join("node").to_string_lossy().into_owned(),
        helper: Some(Helper::Path(helper(folder))),
        batch_endpoint: fake.address.clone(),
        logging_endpoint: fake.address.clone(),
        ..Settings::default()
    };
    let project = credentials.project(None, |_| None).expect("a project");
    let location = settings.location().to_owned();
    let client = Client::new(project, location, credentials, &settings, Duration::ZERO);
    let stage = RemoteStage {
        objects: content_store::Store::local(&folder.join("staging")).expect("a store"),
        expiry: Duration::from_secs(600),
        max_outputs: 4,
        credentials: false,
        cleanup: true,
    };
    let mut backend = GoogleBackend::new(client, settings, folder.join("scratch"), stage, None)
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

fn placement() -> Placement {
    Placement {
        cpus: 2,
        memory_bytes: Some(3 << 30),
        time_seconds: Some(3600),
        labels: [("team", "genomics")]
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .into(),
        ..Placement::default()
    }
}

/// The claims of the JWT that a token request sent, after its signature is checked.
fn claims(request: &common::Request) -> Value {
    let form = String::from_utf8_lossy(&request.body).into_owned();
    let assertion = (form.split('&'))
        .find_map(|pair| pair.strip_prefix("assertion="))
        .expect("an assertion");
    let parts: Vec<&str> = assertion.split('.').collect();
    let signed = format!("{}.{}", parts[0], parts[1]);
    let signature = URL_SAFE_NO_PAD.decode(parts[2]).expect("base64");
    let signature = rsa::pkcs1v15::Signature::try_from(signature.as_slice()).expect("a signature");
    let verifier = rsa::pkcs1v15::VerifyingKey::<rsa::sha2::Sha256>::new(key().to_public_key());
    verifier
        .verify(signed.as_bytes(), &signature)
        .expect("a valid signature");
    let header: Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).expect("base64")).expect("JSON");
    assert_eq!(
        header,
        json!({"alg": "RS256", "typ": "JWT", "kid": "key-1"})
    );
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).expect("base64")).expect("JSON")
}

/// Runs a task with an input through the fake: the token comes from a signed JWT, and the job
/// holds the resources, spot settings and labels of the task.
#[tokio::test]
async fn runs_staged_jobs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeGoogle::start(&folder.path().join("fake"));
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
    let tokens = fake.requests("POST /token");
    assert_eq!(tokens.len(), 1, "one token serves all calls");
    let form = String::from_utf8_lossy(&tokens[0].body).into_owned();
    assert!(form.starts_with("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&"));
    let claims = claims(&tokens[0]);
    assert_eq!(claims["iss"], "uwe@lab.iam.gserviceaccount.com");
    assert_eq!(claims["aud"], format!("{}/token", fake.address));
    assert_eq!(
        claims["scope"],
        "https://www.googleapis.com/auth/cloud-platform"
    );
    assert_eq!(
        claims["exp"].as_u64(),
        claims["iat"].as_u64().map(|iat| iat + 3600)
    );
    let created = fake.requests("POST /v1/projects/lab/locations/europe-west4/jobs?job_id=");
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].authorization, "Bearer token-1");
    let id = created[0].path.rsplit('=').next().expect("an id");
    assert!(id.starts_with("uwe-run-align-1-1-"), "{id}");
    let body = created[0].json();
    let spec = &body["taskGroups"][0]["taskSpec"];
    assert_eq!(
        spec["computeResource"],
        json!({"cpuMilli": 2000, "memoryMib": 3072})
    );
    assert_eq!(spec["maxRunDuration"], "3600s");
    assert_eq!(spec["maxRetryCount"], 2);
    let container = &spec["runnables"][0]["container"];
    assert_eq!(container["imageUri"], IMAGE);
    let folder_text = folder.path().to_string_lossy();
    let volumes = json!([
        format!("{folder_text}/node:{folder_text}/node"),
        format!("{folder_text}:{folder_text}:ro"),
    ]);
    assert_eq!(container["volumes"], volumes);
    let allocation = &body["allocationPolicy"];
    assert_eq!(
        allocation["instances"][0]["policy"]["provisioningModel"],
        "SPOT"
    );
    assert_eq!(
        allocation["serviceAccount"]["email"],
        "jobs@lab.iam.gserviceaccount.com"
    );
    assert_eq!(body["labels"], json!({"team": "genomics"}));
    assert!(
        !fake
            .requests(&format!(
                "GET /v1/projects/lab/locations/europe-west4/jobs/{id}"
            ))
            .is_empty()
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

/// A cancelled task deletes its job, which stops it.
#[tokio::test]
async fn cancels_running_jobs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeGoogle::start(&folder.path().join("fake"));
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
    assert_eq!(fake.requests("DELETE ").len(), 1);
    assert!(
        fake.requests("DELETE ")[0]
            .path
            .ends_with(&format!("/jobs/{id}"))
    );
    // The fake notes the end of the killed runnable when its thread has waited for it.
    for _ in 0..3000 {
        if !fake.running(&id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!fake.running(&id));
}

/// A job that cannot even stage fails with the end of its Cloud Logging log, and a throttled
/// creation is sent again.
#[tokio::test]
async fn reports_job_logs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeGoogle::start(&folder.path().join("fake"));
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
    assert_eq!(engine, "Google Batch");
    assert!(message.contains("exit code 125"), "{message}");
    assert!(message.contains("/nonexistent/uwe"), "{message}");
    assert_eq!(fake.requests("POST /v1/").len(), 3);
    let listed = fake.requests("POST /v2/entries:list")[0].json();
    assert_eq!(listed["resourceNames"], json!(["projects/lab"]));
    let filter = listed["filter"].as_str().expect("a filter");
    assert!(filter.ends_with("labels.job_uid=\"uid-1\""), "{filter}");
}

/// A task whose spot VM went away fails as a failure of the machine, which uwe may retry.
#[tokio::test]
async fn reports_preempted_jobs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeGoogle::start(&folder.path().join("fake"));
    fake.end_with(50001);
    let backend = backend(folder.path(), &fake);
    let spec = spec("echo hi > out.txt");
    let placement = placement();
    let staging = Staging::default();
    let job = Job {
        name: "run-SPOT-1-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let ended = backend.run(job, &CancellationToken::new()).await;
    let Err(ExecutorError::Engine { message, .. }) = ended else {
        panic!("a failure of the machine: {ended:?}");
    };
    assert!(message.contains("preempted (exit code 50001)"), "{message}");
}

/// Stages a task through a Google Cloud Storage bucket with presigned requests, as on Google
/// Cloud, while the fake runs the job here.
#[tokio::test]
#[ignore = "needs fake-gcs-server; run `just emulator-test`"]
async fn stages_through_gcs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeGoogle::start(&folder.path().join("fake"));
    let mut backend = backend(folder.path(), &fake);
    let root = std::env::var("UWE_GCS_ROOT").expect("UWE_GCS_ROOT is set");
    let root = format!("{root}/uwe-google-batch-{}", std::process::id());
    let text = std::env::var("UWE_TEST_STORAGE").expect("UWE_TEST_STORAGE is set");
    let config =
        config_file::ConfigFile::parse(&text, Path::new("storage.toml")).expect("a config");
    let storage = storage::Storage::from_config(&config).expect("storage providers");
    let storage::StoreLocation::Objects(operator) = storage.store(&root).expect("a store") else {
        panic!("{root} is not in an object store");
    };
    backend.stage.objects = content_store::Store::remote(operator, &root).expect("a store");
    let source = folder.path().join("in.txt");
    std::fs::write(&source, "hello gcs\n").expect("writes the input");
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
        name: "run-GCS-1-1",
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
    assert_eq!(output, "HELLO GCS\n");
    let body = fake.requests("POST /v1/")[0].json();
    let commands = &body["taskGroups"][0]["taskSpec"]["runnables"][0]["container"]["commands"];
    let plan = commands[5].as_str().expect("the plan");
    assert!(
        plan.starts_with("http") && plan.contains("X-Goog-Signature"),
        "{plan}"
    );
}
