//! Checks task outputs and container ordering against a local Funnel TES server.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use executor::{Backend, ExecutorError, Job, Staged, Staging};
use task_model::{ContainerSpec, FORMAT, Image, OutputRule, Placement, TaskSpec};
use tes_executor::{Client, TesBackend};
use tokio_util::sync::CancellationToken;

fn backend(folder: &std::path::Path) -> TesBackend {
    let server = std::env::var("IUNCTUS_TES_SERVER").expect("IUNCTUS_TES_SERVER names Funnel");
    // Funnel runs images of this machine, whose configs no registry needs to give.
    let config = executor::registry::ImageConfig::default();
    TesBackend {
        client: Client::new(&server, None).expect("a client"),
        scratch: folder.join("scratch"),
        staging: None,
        settings: Default::default(),
        configs: executor::registry::ImageConfigs::known([
            ("debian:stable-slim".into(), config.clone()),
            ("alpine:latest".into(), config),
        ]),
    }
}

fn spec(script: &str) -> TaskSpec {
    TaskSpec {
        run_as: None,
        format: FORMAT,
        namespace: String::new(),
        image: Some(Image {
            reference: "debian:stable-slim".into(),
            platform: None,
        }),
        arguments: vec!["bash".into(), "-c".into(), script.into()],
        workdir: None,
        environment: Default::default(),
        inputs: Vec::new(),
        outputs: vec![OutputRule {
            pattern: "out/*.txt".into(),
        }],
        stdin: None,
        containers: Vec::new(),
        replaces_entrypoint: false,
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
}

#[test]
#[ignore = "needs a Funnel server; run with `just tes-test`"]
fn funnel_returns_outputs() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let source = folder.path().join("in.txt");
    std::fs::write(&source, "hello\n").expect("writes the input");
    let staging = Staging {
        inputs: vec![Staged {
            path: "in.txt".into(),
            source,
            directory: false,
            copy: false,
            executable: false,
            ..Default::default()
        }],
        ..Staging::default()
    };
    let script = "mkdir out && tr a-z A-Z < in.txt > out/a.txt && echo 2 > out/b.txt";
    let (failing, working) = (spec(&format!("{script}; exit 3")), spec(script));
    let placement = Placement::default();
    let job = |name, spec| Job {
        name,
        spec,
        staging: &staging,
        placement: &placement,
    };
    let backend = backend(folder.path());
    let run = |job| runtime().block_on(backend.run(job, &CancellationToken::new()));
    // A tool that fails gives its exit code, and no outputs.
    let finished = run(job("run-P-1-1", &failing)).expect("runs");
    assert_eq!(finished.exit_code, Some(3));
    assert!(finished.files.is_empty());
    let finished = run(job("run-P-1-2", &working)).expect("runs");
    assert_eq!(finished.exit_code, Some(0));
    assert_eq!(finished.files.len(), 2);
    let output = std::fs::read_to_string(finished.local_path("out/a.txt")).expect("reads");
    assert_eq!(output, "HELLO\n");
}

#[test]
#[ignore = "needs a Funnel server; run with `just tes-test`"]
fn funnel_cancels_tasks() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let spec = spec("sleep 120");
    let staging = Staging::default();
    let job = Job {
        name: "run-P-2-1",
        spec: &spec,
        staging: &staging,
        placement: &Placement::default(),
    };
    let backend = backend(folder.path());
    let cancel = CancellationToken::new();
    let stopper = {
        let cancel = cancel.clone();
        async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            cancel.cancel();
        }
    };
    let (result, ()) =
        runtime().block_on(async { tokio::join!(backend.run(job, &cancel), stopper) });
    assert!(
        matches!(result, Err(ExecutorError::Cancelled)),
        "{result:?}"
    );
    assert!(!folder.path().join("scratch/run-P-2-1").exists());
}

#[test]
#[ignore = "needs a Funnel server and the alpine:latest image; run with `just tes-test`"]
fn funnel_refuses_containers() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let container = |name: &str, image: &str, script: &str, after: &[&str]| ContainerSpec {
        service: false,
        replaces_entrypoint: false,
        name: name.into(),
        image: Some(Image {
            reference: image.into(),
            platform: None,
        }),
        arguments: vec!["sh".into(), "-c".into(), script.into()],
        workdir: None,
        environment: Default::default(),
        after: after.iter().map(|name| (*name).to_owned()).collect(),
        attempts: 1,
    };
    let script = "mkdir -p out && cat first.txt > out/a.txt && echo main";
    let main = container("main", "debian:stable-slim", script, &["first"]);
    let first = container(
        "first",
        "alpine:latest",
        "cat /etc/alpine-release > first.txt",
        &[],
    );
    let spec = TaskSpec {
        arguments: main.arguments.clone(),
        containers: vec![main, first],
        ..spec("")
    };
    let (staging, placement) = (Staging::default(), Placement::default());
    let job = Job {
        name: "run-S-1-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let backend = backend(folder.path());
    assert!(matches!(
        runtime().block_on(backend.run(job, &CancellationToken::new())),
        Err(ExecutorError::Unsupported(_))
    ));
}

#[test]
#[ignore = "needs a TES server and S3 bucket accessible to both client and worker"]
fn transfers_s3_outputs() {
    use config_file::ConfigFile;
    use content_store::Store;
    use storage::Storage;
    use tes_executor::{ObjectStage, Settings};

    let server = std::env::var("IUNCTUS_TES_SERVER").expect("IUNCTUS_TES_SERVER");
    let root = std::env::var("IUNCTUS_TES_STAGING_ROOT").expect("IUNCTUS_TES_STAGING_ROOT");
    let endpoint = std::env::var("IUNCTUS_S3_ENDPOINT").expect("IUNCTUS_S3_ENDPOINT");
    let text = format!(
        "[storage.tes]\ntype = 's3'\nprefixes = ['s3://']\nendpoint = {}\nregion = 'us-east-1'\naccess_key_id = {{ environment = 'AWS_ACCESS_KEY_ID' }}\nsecret_access_key = {{ environment = 'AWS_SECRET_ACCESS_KEY' }}\n",
        serde_json::to_string(&endpoint).unwrap()
    );
    let config = ConfigFile::parse(&text, std::path::Path::new("tes-test.toml")).unwrap();
    let storage = Storage::from_config(&config).unwrap();
    let folder = tempfile::tempdir().unwrap();
    let source = Store::local(&folder.path().join("store")).unwrap();
    let settings = Settings {
        staging_root: Some(root),
        ..Settings::default()
    };
    let backend = TesBackend {
        client: Client::new(&server, std::env::var("IUNCTUS_TES_TOKEN").ok()).unwrap(),
        staging: Some(ObjectStage::configured(source, &storage, &settings).unwrap()),
        settings,
        ..backend(folder.path())
    };
    let input = folder.path().join("input");
    let directory = folder.path().join("directory");
    std::fs::write(&input, b"alpha\n").unwrap();
    std::fs::create_dir_all(directory.join("nested")).unwrap();
    std::fs::write(directory.join("nested/b.txt"), b"beta\n").unwrap();
    let staging = Staging {
        inputs: vec![
            Staged {
                path: "in.txt".into(),
                source: input,
                ..Staged::default()
            },
            Staged {
                path: "directory".into(),
                source: directory,
                directory: true,
                ..Staged::default()
            },
        ],
        ..Staging::default()
    };
    let mut spec = spec(
        "mkdir -p out tree/deep empty && cat in.txt directory/nested/b.txt > out/a.txt && echo hidden > tree/deep/.hidden && echo captured",
    );
    spec.replaces_entrypoint = true;
    spec.outputs.extend([
        OutputRule {
            pattern: "tree/".into(),
        },
        OutputRule {
            pattern: "empty/".into(),
        },
    ]);
    let placement = Placement::default();
    let name = format!(
        "s3-{}",
        task_model::Digest::sha256(folder.path().to_string_lossy().as_bytes()).hex()
    );
    let finished = runtime()
        .block_on(backend.run(
            Job {
                name: &name,
                spec: &spec,
                staging: &staging,
                placement: &placement,
            },
            &CancellationToken::new(),
        ))
        .unwrap();
    assert_eq!(finished.exit_code, Some(0));
    assert_eq!(
        std::fs::read(finished.local_path("out/a.txt")).unwrap(),
        b"alpha\nbeta\n"
    );
    assert_eq!(
        std::fs::read(finished.local_path("tree/deep/.hidden")).unwrap(),
        b"hidden\n"
    );
    assert!(finished.local_path("empty").is_dir());
    assert_eq!(std::fs::read(finished.stdout()).unwrap(), b"captured\n");
}
