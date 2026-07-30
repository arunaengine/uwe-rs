//! The TES backend against a real Funnel server on this machine, which `just funnel-server`
//! starts. Needs Docker and the `debian:stable-slim` image; run with `just tes-test`, which
//! sets `IUNCTUS_TES_SERVER`. Funnel must be allowed to read and write the temporary folder.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use executor::{Backend, ExecutorError, Job, Staged, Staging};
use task_model::{FORMAT, Image, OutputRule, Placement, TaskSpec};
use tes_executor::{Client, TesBackend};
use tokio_util::sync::CancellationToken;

fn backend(folder: &std::path::Path) -> TesBackend {
    let server = std::env::var("IUNCTUS_TES_SERVER").expect("IUNCTUS_TES_SERVER names Funnel");
    TesBackend {
        client: Client::new(&server, None).expect("a client"),
        scratch: folder.join("scratch"),
    }
}

fn spec(script: &str) -> TaskSpec {
    TaskSpec {
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
fn funnel_runs_tasks_and_gives_back_their_outputs() {
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
