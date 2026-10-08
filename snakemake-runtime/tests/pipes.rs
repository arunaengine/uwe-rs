//! FIFO jobs share one concurrent container task and fail and retry together.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{Launch, Outcome};

const SNAKEFILE: &str = r#"
rule all:
    input: "independent.txt", "final.txt"
rule independent:
    output: "independent.txt"
    shell: "touch {output}"
rule produce:
    retries: 1
    output: pipe("stream.txt")
    shell: "echo bytes > stream.txt"
rule consume:
    input: "stream.txt"
    output: "final.txt"
    shell: "cat stream.txt > final.txt"
"#;

#[test]
fn groups_pipe_tasks() {
    for existing in [false, true] {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("Snakefile"), SNAKEFILE).unwrap();
        if existing {
            std::fs::write(folder.path().join("stream.txt"), "old").unwrap();
        }
        let mut driver = snakemake_runtime::driver(&Launch {
            target: folder.path().to_path_buf(),
            scratch: folder.path().join("scratch"),
            ..Launch::default()
        })
        .unwrap();
        let tasks = driver.start().unwrap();
        assert_eq!(tasks.len(), 2);
        let task = tasks
            .iter()
            .find(|task| task.name.contains("produce"))
            .unwrap();
        assert_eq!(task.outputs, ["final.txt"]);
        assert!(task.inputs.is_empty());
        assert_eq!(task.resources.cpus, Some(2.0));
        assert!(task.command[2].contains("mkfifo stream.txt"));
        let again = driver
            .complete(Outcome {
                id: task.id,
                attempt: 1,
                exit_code: 7,
                ..Outcome::default()
            })
            .unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].id, task.id);
        assert_eq!(again[0].outputs, task.outputs);
        assert!(
            driver
                .complete(Outcome {
                    id: task.id,
                    attempt: 2,
                    exit_code: 9,
                    ..Outcome::default()
                })
                .unwrap()
                .is_empty()
        );
        assert!(driver.finish().is_err());
        assert!(driver.deliveries().is_empty());
    }
}

#[test]
fn rejects_pipe_images() {
    let folder = tempfile::tempdir().unwrap();
    let workflow = SNAKEFILE
        .replace("rule produce:", "rule produce:\n    container: 'first'")
        .replace("rule consume:", "rule consume:\n    container: 'second'");
    std::fs::write(folder.path().join("Snakefile"), workflow).unwrap();
    let mut driver = snakemake_runtime::driver(&Launch {
        target: folder.path().into(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    })
    .unwrap();
    let error = driver.start().unwrap_err();
    assert!(error.unsupported);
    assert!(error.message.contains("must run together"));
    assert!(error.message.contains("same container image"));
    assert!(driver.deliveries().is_empty());
}

#[test]
fn connects_pipe_chains() {
    let folder = tempfile::tempdir().unwrap();
    let workflow = SNAKEFILE.replace("rule consume:", "rule middle:\n    input: 'stream.txt'\n    output: pipe('next.txt')\n    shell: 'cat {input} > {output}'\nrule consume:").replace("rule consume:\n    input: \"stream.txt\"", "rule consume:\n    input: \"next.txt\"");
    std::fs::write(folder.path().join("Snakefile"), workflow).unwrap();
    let mut driver = snakemake_runtime::driver(&Launch {
        target: folder.path().into(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    })
    .unwrap();
    let tasks = driver.start().unwrap();
    let task = tasks
        .iter()
        .find(|task| task.name.contains("produce"))
        .unwrap();
    assert!(task.name.contains("middle"));
    assert_eq!(task.outputs, ["final.txt"]);
    assert!(task.command[2].contains("mkfifo next.txt"));
}

#[test]
#[ignore = "needs the already local debian:stable-slim image"]
fn runs_local_pipes() {
    use executor::process::{ContainerSpec, Mount, run_to_end};
    use std::os::unix::fs::MetadataExt;
    for (number, workflow) in [
        SNAKEFILE.to_owned(),
        SNAKEFILE.replace("echo bytes > stream.txt", "exit 7"),
        SNAKEFILE.replace("cat stream.txt > final.txt", "exit 9"),
    ]
    .iter()
    .enumerate()
    {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("Snakefile"), workflow).unwrap();
        let mut driver = snakemake_runtime::driver(&Launch {
            target: folder.path().into(),
            scratch: folder.path().join("scratch"),
            ..Launch::default()
        })
        .unwrap();
        let tasks = driver.start().unwrap();
        let task = tasks
            .iter()
            .find(|task| task.name.contains("produce"))
            .unwrap();
        let result = run_to_end(&ContainerSpec {
            name: format!("iunctus-pipe-{}-{number}", std::process::id()),
            image: "debian:stable-slim".into(),
            user: Some(format!(
                "{}:{}",
                folder.path().metadata().unwrap().uid(),
                folder.path().metadata().unwrap().gid()
            )),
            entrypoint: Some("/usr/bin/timeout".into()),
            command: vec![
                "10".into(),
                "bash".into(),
                "-c".into(),
                task.command[2].clone(),
            ],
            mounts: vec![Mount {
                source: folder.path().into(),
                target: "/work".into(),
                read_only: false,
            }],
            workdir: Some("/work".into()),
            memory: Some(64 << 20),
            ..ContainerSpec::default()
        });
        if number == 0 {
            result.unwrap();
            assert_eq!(
                std::fs::read_to_string(folder.path().join("final.txt")).unwrap(),
                "bytes\n"
            );
        } else {
            assert!(result.is_err());
        }
    }
}

#[test]
fn preserves_unused_pipes() {
    for unused in [false, true] {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("Snakefile"), SNAKEFILE).unwrap();
        if !unused {
            for file in ["stream.txt", "final.txt", "independent.txt"] {
                std::fs::write(folder.path().join(file), "old").unwrap();
            }
        }
        let mut driver = snakemake_runtime::driver(&Launch {
            target: folder.path().to_path_buf(),
            scratch: folder.path().join("scratch"),
            targets: if unused {
                vec!["independent".into()]
            } else {
                Vec::new()
            },
            ..Launch::default()
        })
        .unwrap();
        let tasks = driver.start().unwrap();
        assert!(
            tasks
                .iter()
                .all(|task| !["produce", "consume"].contains(&task.step.as_str()))
        );
    }
}
