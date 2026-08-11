//! Right-sized resources in real runs: a run after an earlier one asks for the memory that its
//! tasks used, not for what the workflow names, and says so; `-no-right-sizing` keeps what the
//! workflow names. `cargo test -p iunctus --test efficiency -- --ignored` (needs Docker and the
//! debian:stable-slim image).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

mod common;

/// A step that asks for 4 GB and writes the memory limit of its container.
const SCRIPT: &str = r#"
process LIMIT {
    container 'debian:stable-slim'
    memory '4 GB'
    publishDir 'results', mode: 'copy'
    input:
    val n
    output:
    path "limit-${n}.txt"
    script:
    """
    cat /sys/fs/cgroup/memory.max > limit-${n}.txt
    """
}

workflow {
    LIMIT(channel.of(1, 2))
}
"#;

/// Runs the binary in `directory`; the result has the exit code and the standard output and
/// error.
fn iunctus(directory: &Path, arguments: &[&str]) -> (i32, String) {
    let output = common::command()
        .args(arguments)
        .current_dir(directory)
        .env("XDG_CACHE_HOME", directory.join("cache"))
        .output()
        .expect("runs iunctus");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    (
        output.status.code().unwrap_or(-1),
        format!("{stdout}{stderr}"),
    )
}

/// The memory limit that the task of `n` saw in its container, in bytes.
fn limit(directory: &Path, n: u32) -> u64 {
    let path = directory.join(format!("results/limit-{n}.txt"));
    let text = std::fs::read_to_string(&path).expect("reads the limit");
    text.trim().parse().expect("a number of bytes")
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn a_later_run_asks_for_the_memory_that_its_tasks_used() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let directory = directory.path();
    std::fs::write(directory.join("main.nf"), SCRIPT).expect("writes the workflow");
    let asked = 4 * 1024 * 1024 * 1024;

    let (code, output) = iunctus(directory, &["run", "main.nf", "-executor", "local"]);
    assert_eq!(code, 0, "{output}");
    assert!(!output.contains("asked for less"), "{output}");
    assert_eq!(limit(directory, 1), asked);

    // The cache would answer every task, so the next runs leave it out.
    let again = ["run", "main.nf", "-executor", "local", "-no-cache"];
    let (code, output) = iunctus(directory, &again);
    assert_eq!(code, 0, "{output}");
    assert!(
        output.contains("tasks asked for less than the workflow")
            && output.contains("LIMIT (2 tasks): 67.1 MB of memory instead of 4.3 GB"),
        "{output}"
    );
    assert_eq!(limit(directory, 1), task_model::LEAST_MEMORY);
    assert_eq!(limit(directory, 2), task_model::LEAST_MEMORY);

    let kept = [&again[..], &["-no-right-sizing"]].concat();
    let (code, output) = iunctus(directory, &kept);
    assert_eq!(code, 0, "{output}");
    assert!(!output.contains("asked for less"), "{output}");
    assert_eq!(limit(directory, 2), asked);

    // A simulated run says what a real run would ask for.
    let (code, output) = iunctus(directory, &["run", "main.nf", "-no-cache"]);
    assert_eq!(code, 0, "{output}");
    assert!(
        output.contains("a real run would ask for less than the workflow"),
        "{output}"
    );
}
