//! Checks the Azure Batch backend against a local fake of Azure Batch and the Entra ID token
//! endpoints that runs each task's command here.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use azure_batch_executor::{
    AzureBackend, Client, Credentials, Hidden, Identity, PoolSettings, Settings, Source,
};
use executor::batch::Helper;
use executor::remote::RemoteStage;
use executor::{Backend, ExecutorError, Job, Staged, Staging};
use serde_json::json;
use sha2::{Digest, Sha256};
use task_model::{FORMAT, Image, OutputRule, Placement, TaskSpec};
use tokio_util::sync::CancellationToken;

use common::{ACCOUNT, FakeAzure, KEY};

/// An image with a digest, which needs no registry.
const IMAGE: &str =
    "ubuntu@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The test program as the helper of the tasks: it runs `stage_helper` with the arguments.
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

/// A backend in `folder` whose calls go to the fake with the Shared Key of its account, and
/// whose tasks run on the pool `small`.
fn backend(folder: &Path, fake: &FakeAzure, credentials: Credentials) -> AzureBackend {
    let settings = Settings {
        account: Some(ACCOUNT.into()),
        endpoint: Some(fake.address.clone()),
        pool: Some("small".into()),
        poll: Duration::from_millis(50),
        quota_wait: Duration::ZERO,
        task_folder: folder.join("node").to_string_lossy().into_owned(),
        helper: Some(Helper::Path(helper(folder))),
        ..Settings::default()
    };
    let api = settings.api_version.clone();
    let client = Client::new(fake.address.clone(), ACCOUNT.into(), api, credentials);
    let stage = RemoteStage {
        objects: content_store::Store::local(&folder.join("staging")).expect("a store"),
        expiry: Duration::from_secs(600),
        max_outputs: 4,
        credentials: false,
        cleanup: true,
    };
    let mut backend = AzureBackend::new(client, settings, folder.join("scratch"), stage, None)
        .expect("a backend");
    backend.staging_time = Duration::ZERO;
    backend
}

fn shared_key() -> Credentials {
    let settings = Settings {
        account_key: Some(Hidden::Text(KEY.into())),
        ..Settings::default()
    };
    Credentials::find(&settings, ACCOUNT, |_| None).expect("a Shared Key")
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

/// Runs a task with an input on an existing pool, signed with the Shared Key: the task holds
/// the resources of the task and the plan as a resource file, and the run ends its job.
#[tokio::test]
async fn runs_staged_tasks() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeAzure::start(&folder.path().join("fake"));
    fake.add_pool("small", 4);
    let backend = backend(folder.path(), &fake, shared_key());
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
        .expect("the task ends");
    assert_eq!(finished.exit_code, Some(0));
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("the output");
    assert_eq!(output, "HELLO\n");
    assert_eq!(
        fake.unsigned(),
        Vec::<String>::new(),
        "every signature matches"
    );
    let jobs = fake.requests("POST /jobs?");
    assert_eq!(jobs.len(), 1);
    let created = jobs[0].json();
    assert_eq!(created["poolInfo"]["poolId"], "small");
    assert_eq!(created["constraints"]["maxWallClockTime"], "PT2592000S");
    let id = created["id"].as_str().expect("a job id");
    assert!(id.starts_with("uwe-") && id.ends_with("-small"), "{id}");
    let tasks = fake.requests(&format!("POST /jobs/{id}/tasks?"));
    assert_eq!(tasks.len(), 1);
    assert!(
        tasks[0]
            .header("authorization")
            .starts_with("SharedKey lab:")
    );
    assert_eq!(
        tasks[0].header("content-type"),
        "application/json; odata=minimalmetadata"
    );
    let task = tasks[0].json();
    assert_eq!(task["id"], "uwe-run-ALIGN-1-1");
    assert_eq!(task["requiredSlots"], 2);
    assert_eq!(task["constraints"]["maxWallClockTime"], "PT3600S");
    let container = &task["containerSettings"];
    assert_eq!(container["imageName"], IMAGE);
    let options = container["containerRunOptions"].as_str().expect("options");
    assert!(
        options.starts_with("--cpu-shares 2048 --memory 3072m -v "),
        "{options}"
    );
    assert_eq!(task["resourceFiles"][0]["filePath"], "uwe-plan.json");
    assert_eq!(
        task["userIdentity"],
        json!({"autoUser": {"scope": "task", "elevationLevel": "nonadmin"}})
    );
    assert_eq!(
        fake.requests("DELETE /jobs/").len(),
        1,
        "the successful task goes away"
    );
    backend.release().await;
    let patched = fake.requests(&format!("PATCH /jobs/{id}?"));
    assert_eq!(
        patched[0].json(),
        json!({"onAllTasksComplete": "terminatejob", "poolInfo": {"poolId": "small"}})
    );
    assert!(
        fake.requests("DELETE /pools/").is_empty(),
        "uwe keeps pools it did not create"
    );
}

/// With auto pools, uwe creates a pool of a verified image with a service principal's token,
/// waits while the job quota is full, gets the helper by its URL and deletes the pool at the
/// end of the run.
#[tokio::test]
async fn creates_auto_pools() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeAzure::start(&folder.path().join("fake"));
    fake.fill_quota(1);
    let source = Source::Secret {
        url: format!("{}/tenant/oauth2/v2.0/token", fake.address),
        client_id: "client".into(),
        secret: "made-up".into(),
    };
    let mut backend = backend(folder.path(), &fake, Credentials::tokens(source));
    let script = helper(folder.path());
    let sha256: String = Sha256::digest(std::fs::read(&script).expect("the helper"))
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let settings = &mut backend.settings;
    settings.helper = Some(Helper::Download {
        url: format!("file://{script}"),
        sha256,
    });
    (settings.pool, settings.auto_pools, settings.delete_pools) = (None, true, true);
    let auto = PoolSettings {
        vm_size: "Standard_E2d_v5".into(),
        vm_cpus: Some(2),
        auto_scale: true,
        ..PoolSettings::default()
    };
    settings.pools.insert("auto".into(), auto);
    let spec = spec("echo auto > out.txt");
    let placement = placement();
    let staging = Staging::default();
    let job = Job {
        name: "run-AUTO-1-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let finished = backend
        .run(job, &CancellationToken::new())
        .await
        .expect("the task ends");
    assert_eq!(finished.exit_code, Some(0));
    let tokens = fake.requests("POST /tenant/oauth2/v2.0/token");
    assert_eq!(tokens.len(), 1, "one token serves all calls");
    let form = String::from_utf8_lossy(&tokens[0].body).into_owned();
    assert!(form.contains("grant_type=client_credentials"), "{form}");
    assert!(
        form.contains("scope=https%3A%2F%2Fbatch.core.windows.net%2F.default"),
        "{form}"
    );
    let pools = fake.requests("POST /pools?");
    assert_eq!(pools.len(), 1);
    assert_eq!(pools[0].header("authorization"), "Bearer token-1");
    let pool = pools[0].json();
    let id = pool["id"].as_str().expect("a pool id").to_owned();
    assert!(
        id.starts_with("uwe-pool-") && id.ends_with("-Standard_E2d_v5"),
        "{id}"
    );
    assert_eq!(
        pool["virtualMachineConfiguration"]["imageReference"]["sku"],
        "2404"
    );
    assert_eq!(pool["taskSlotsPerNode"], 2);
    assert_eq!(pool["enableAutoScale"], true);
    assert_eq!(
        pool["metadata"],
        json!([{"name": "team", "value": "genomics"}])
    );
    assert_eq!(
        fake.requests("POST /jobs?").len(),
        2,
        "one more try after the quota error"
    );
    let task = fake.requests("POST /jobs/")[0].json();
    assert_eq!(task["resourceFiles"][1]["filePath"], "uwe");
    assert_eq!(task["resourceFiles"][1]["fileMode"], "0755");
    backend.release().await;
    assert_eq!(fake.requests(&format!("DELETE /pools/{id}?")).len(), 1);
}

/// A cancelled task is terminated, with a token from the managed identity of the machine.
#[tokio::test]
async fn cancels_running_tasks() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeAzure::start(&folder.path().join("fake"));
    fake.add_pool("small", 4);
    let mut settings = Settings {
        managed_identity: Some(Identity::User("me".into())),
        ..Settings::default()
    };
    settings.identity_endpoint = format!("{}/metadata/identity/oauth2/token", fake.address);
    let credentials = Credentials::find(&settings, ACCOUNT, |_| None).expect("an identity");
    let backend = backend(folder.path(), &fake, credentials);
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
            if fake.tasks().first().is_some_and(|id| fake.running(id)) {
                cancel.cancel();
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the task never ran");
    };
    let (ended, ()) = tokio::join!(backend.run(job, &cancel), stop);
    assert!(matches!(ended, Err(ExecutorError::Cancelled)), "{ended:?}");
    let identity = fake.requests("GET /metadata/identity/oauth2/token?");
    assert!(
        identity[0].path.contains("client_id=me"),
        "{}",
        identity[0].path
    );
    let stopped = fake.requests("POST /jobs/");
    assert!(
        stopped
            .iter()
            .any(|request| request.path.contains("/tasks/uwe-run-SLOW-1-1/terminate?"))
    );
    assert_eq!(
        stopped
            .last()
            .map(|request| request.header("authorization")),
        Some("Bearer vm-token")
    );
    assert!(!fake.running(&fake.tasks()[0]));
}

/// A task whose helper is missing fails with the end of its output, and stays for a look.
#[tokio::test]
async fn reports_task_logs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeAzure::start(&folder.path().join("fake"));
    fake.add_pool("small", 4);
    let mut backend = backend(folder.path(), &fake, shared_key());
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
    assert_eq!(engine, "Azure Batch");
    assert!(message.contains("exit code 125"), "{message}");
    assert!(message.contains("stderr.txt:\n"), "{message}");
    assert!(message.contains("/nonexistent/uwe"), "{message}");
    assert!(
        fake.requests("DELETE /jobs/").is_empty(),
        "a failed task stays"
    );
    assert_eq!(
        fake.unsigned(),
        Vec::<String>::new(),
        "every signature matches"
    );
}

/// A task on a fixed pool that lost its nodes to resize errors fails and is terminated; a
/// missing pool that uwe may not create fails before any task.
#[tokio::test]
async fn fails_broken_pools() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeAzure::start(&folder.path().join("fake"));
    fake.add_pool("small", 4);
    fake.break_pool("small", "AccountCoreQuotaReached");
    let mut backend = backend(folder.path(), &fake, shared_key());
    backend.pool_check = Duration::ZERO;
    let spec = spec("true");
    let placement = placement();
    let staging = Staging::default();
    let job = Job {
        name: "run-WAIT-1-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let ended = backend.run(job, &CancellationToken::new()).await;
    let Err(ExecutorError::Unsupported(message)) = ended else {
        panic!("a broken pool: {ended:?}");
    };
    assert!(
        message.starts_with("Azure Batch pool 'small' has resize errors"),
        "{message}"
    );
    assert_eq!(
        fake.requests("POST /jobs/")
            .iter()
            .filter(|request| request.path.contains("/terminate?"))
            .count(),
        1
    );
    let missing = Placement {
        queue: Some("missing".into()),
        ..placement
    };
    let job = Job {
        name: "run-WAIT-2-1",
        spec: &spec,
        staging: &staging,
        placement: &missing,
    };
    let ended = backend.run(job, &CancellationToken::new()).await;
    let Err(ExecutorError::Unsupported(message)) = ended else {
        panic!("a missing pool: {ended:?}");
    };
    assert!(
        message.starts_with("Can't find Azure Batch pool 'missing'"),
        "{message}"
    );
}

/// Stages a task through an Azure Blob container with a SAS for each blob, as on Azure, while
/// the fake runs the task here and downloads its resource files from Azurite.
#[tokio::test]
#[ignore = "needs Azurite; run `just emulator-test`"]
async fn stages_through_azure() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeAzure::start(&folder.path().join("fake"));
    fake.add_pool("small", 4);
    let mut backend = backend(folder.path(), &fake, shared_key());
    let root = std::env::var("UWE_AZURE_ROOT").expect("UWE_AZURE_ROOT is set");
    let root = format!("{root}/uwe-azure-batch-{}", std::process::id());
    let text = std::env::var("UWE_TEST_STORAGE").expect("UWE_TEST_STORAGE is set");
    let config =
        config_file::ConfigFile::parse(&text, Path::new("storage.toml")).expect("a config");
    let storage = storage::Storage::from_config(&config).expect("storage providers");
    let storage::StoreLocation::Objects(operator) = storage.store(&root).expect("a store") else {
        panic!("{root} is not in an object store");
    };
    backend.stage.objects = content_store::Store::remote(operator, &root).expect("a store");
    let source = folder.path().join("in.txt");
    std::fs::write(&source, "hello azure\n").expect("writes the input");
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
        name: "run-BLOB-1-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let finished = backend
        .run(job, &CancellationToken::new())
        .await
        .expect("the task ends");
    assert_eq!(finished.exit_code, Some(0));
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("the output");
    assert_eq!(output, "HELLO AZURE\n");
    let task = fake.requests("POST /jobs/")[0].json();
    let plan = task["resourceFiles"][0]["httpUrl"]
        .as_str()
        .expect("the plan");
    assert!(plan.starts_with("http") && plan.contains("sig="), "{plan}");
}
