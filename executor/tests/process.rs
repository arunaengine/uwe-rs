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
