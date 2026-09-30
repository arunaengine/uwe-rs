//! Unsupported FIFO execution is refused before any producer task is published.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::Launch;

const SNAKEFILE: &str = r#"
rule all:
    input: "independent.txt", "final.txt"
rule independent:
    output: "independent.txt"
    shell: "touch {output}"
rule produce:
    output: pipe("stream.txt")
    shell: "mkfifo stream.txt; echo bytes > stream.txt"
rule consume:
    input: "stream.txt"
    output: "final.txt"
    shell: "cat stream.txt > final.txt"
"#;

#[test]
fn rejects_pipe_tasks() {
    for existing_pipe in [false, true] {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("Snakefile"), SNAKEFILE).unwrap();
        if existing_pipe {
            std::fs::write(folder.path().join("stream.txt"), "old").unwrap();
        }
        let mut driver = snakemake_runtime::driver(&Launch {
            target: folder.path().to_path_buf(),
            scratch: folder.path().join("scratch"),
            ..Launch::default()
        })
        .unwrap();
        let error = driver.start().unwrap_err();
        assert!(error.unsupported);
        assert!(
            error
                .message
                .contains("pipe output stream.txt of rule produce")
        );
        assert!(
            error
                .message
                .contains("FIFO creation and concurrent producer/consumer execution")
        );
        assert!(driver.deliveries().is_empty());
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
