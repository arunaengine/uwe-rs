//! Runs tasks in containers with the local Docker. Needs Docker and the `debian:stable-slim`
//! image; run with `cargo test -p executor --test local -- --ignored`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use executor::local::{ExecutorError, LocalContainer, Staged};
use task_model::{FORMAT, Image, OutputRule, TaskSpec};

fn spec(image: Option<&str>, outputs: &[&str]) -> TaskSpec {
    TaskSpec {
        format: FORMAT,
        namespace: String::new(),
        image: image.map(|reference| Image {
            reference: reference.to_owned(),
            platform: None,
        }),
        arguments: vec!["/bin/bash".into(), "-ue".into(), ".command.sh".into()],
        environment: BTreeMap::new(),
        inputs: Vec::new(),
        outputs: outputs
            .iter()
            .map(|pattern| OutputRule {
                pattern: (*pattern).to_owned(),
            })
            .collect(),
        stdin: None,
        stdout: ".command.out".into(),
        stderr: ".command.err".into(),
    }
}

fn stage(root: &std::path::Path, name: &str, text: &str) -> Staged {
    let source = root.join(format!("source-{}", name.replace('/', "-")));
    std::fs::write(&source, text).expect("writes the input");
    Staged {
        path: name.to_owned(),
        source,
        executable: false,
    }
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn runs_a_task_in_a_container() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let executor = LocalContainer {
        engine: "docker".into(),
        scratch: root.path().join("scratch"),
    };
    let staged = [
        stage(
            root.path(),
            ".command.sh",
            "cat in/data.txt > out.txt\necho done\n",
        ),
        stage(root.path(), "in/data.txt", "hello\n"),
    ];
    let finished = executor
        .run(
            "task-1",
            &spec(Some("debian:stable-slim"), &["out.txt"]),
            &staged,
        )
        .expect("runs");
    assert_eq!(finished.exit_code, Some(0));
    let paths: Vec<&str> = finished
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(paths, [".command.out", ".command.err", "out.txt"]);
    let output = std::fs::read_to_string(finished.work.join("out.txt")).expect("reads the output");
    assert_eq!(output, "hello\n");
    let owner = std::os::unix::fs::MetadataExt::uid(
        &std::fs::metadata(finished.work.join("out.txt")).unwrap(),
    );
    let me = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(root.path()).unwrap());
    assert_eq!(owner, me, "outputs belong to the current user");
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn reports_failures() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let executor = LocalContainer {
        engine: "docker".into(),
        scratch: root.path().join("scratch"),
    };
    let staged = [stage(
        root.path(),
        ".command.sh",
        "echo broken >&2\nexit 4\n",
    )];
    let finished = executor
        .run(
            "task-2",
            &spec(Some("debian:stable-slim"), &["out.txt"]),
            &staged,
        )
        .expect("runs");
    assert_eq!(finished.exit_code, Some(4));
    let error = std::fs::read_to_string(finished.work.join(".command.err")).expect("reads the log");
    assert_eq!(error, "broken\n");
    let missing = executor.run("task-3", &spec(None, &[]), &[]).unwrap_err();
    assert!(matches!(missing, ExecutorError::NoImage));
    let error = executor
        .run(
            "task-4",
            &spec(Some("iunctus.invalid/none:0"), &[]),
            &staged,
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("docker could not run the task"),
        "{error}"
    );
}
