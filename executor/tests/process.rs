//! Containers that answer lines, and containers that run to their end, with the local Docker
//! or Podman through its API. Needs the `debian:stable-slim` image; run with
//! `cargo test -p executor --test process -- --ignored`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use executor::process::{ContainerProcess, ContainerSpec, ProcessError, run_to_end};

fn spec(name: &str, script: &str) -> ContainerSpec {
    ContainerSpec {
        engine: std::env::var("IUNCTUS_ENGINE").unwrap_or_default(),
        name: format!("iunctus-test-{name}-{}", std::process::id()),
        image: "debian:stable-slim".into(),
        entrypoint: Some("/bin/bash".into()),
        command: vec!["-c".into(), script.into()],
        memory: Some(64 << 20),
        ..ContainerSpec::default()
    }
}

#[test]
#[ignore = "needs Docker or Podman and the debian:stable-slim image"]
fn containers_answer_lines_until_their_input_closes() {
    let script = "echo ready >&2; while read line; do echo \"got $line\"; done";
    let mut process = ContainerProcess::start(&spec("answers", script)).expect("starts");
    assert_eq!(process.exchange("a").expect("a reply"), "got a");
    assert_eq!(process.exchange("b c").expect("a reply"), "got b c");
    process.stop();
}

#[test]
#[ignore = "needs Docker or Podman and the debian:stable-slim image"]
fn a_name_conflict_does_not_remove_the_existing_container() {
    let settings = spec("conflict", "while read line; do echo \"got $line\"; done");
    let mut first = ContainerProcess::start(&settings).expect("starts");
    assert!(ContainerProcess::start(&settings).is_err());
    assert_eq!(first.exchange("a").unwrap(), "got a");
    assert!(run_to_end(&settings).is_err());
    assert_eq!(first.exchange("b").unwrap(), "got b");
    first.stop();
}

#[test]
#[ignore = "needs Docker or Podman and the debian:stable-slim image"]
fn a_container_that_ends_says_what_it_wrote() {
    let mut process =
        ContainerProcess::start(&spec("ends", "read line; echo gone >&2; exit 3")).expect("starts");
    let error = process.exchange("a").expect_err("no reply");
    assert!(matches!(error, ProcessError::Ended { .. }), "{error}");
    assert!(error.to_string().contains("gone"), "{error}");
}

#[test]
#[ignore = "needs Docker or Podman and the debian:stable-slim image"]
fn containers_run_to_their_end() {
    run_to_end(&spec("done", "true")).expect("runs");
    let error = run_to_end(&spec("fails", "echo broken >&2; exit 2")).expect_err("fails");
    assert!(error.to_string().contains("broken"), "{error}");
}

#[test]
#[ignore = "needs Docker or Podman and the debian:stable-slim image"]
fn a_cancelled_hanging_exchange_removes_its_helper() {
    let settings = spec("hanging-cancel", "read line; sleep 300");
    let cancel = executor::CancellationToken::new();
    let mut process = ContainerProcess::start_cancellable(&settings, &cancel).expect("starts");
    let trigger = cancel.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        trigger.cancel();
    });
    let started = std::time::Instant::now();
    assert!(matches!(
        process.exchange("hang"),
        Err(ProcessError::Cancelled)
    ));
    assert!(started.elapsed() < std::time::Duration::from_secs(12));
    canceller.join().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let engine = runtime.block_on(executor::ContainerEngine::find()).unwrap();
    let error = runtime
        .block_on(engine.client().inspect_container(&settings.name, None))
        .unwrap_err();
    assert!(matches!(
        error,
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            ..
        }
    ));
}
