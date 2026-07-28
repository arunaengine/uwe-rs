//! The insight commands on recorded runs: `iunctus time` and `iunctus compare` on simulated
//! runs, and `iunctus replay`, which refuses simulated runs and, in the ignored test, runs a task
//! of a real run again in its container: `cargo test -p iunctus --test insight -- --ignored`
//! (needs Docker and the debian:stable-slim image).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

mod common;

const SCRIPT: &str = r#"
process UPPER {
    container 'debian:stable-slim'
    input:
    path text
    output:
    path "upper.txt"
    script:
    """
    tr a-z A-Z < ${text} > upper.txt
    """
}

process COUNT {
    container 'debian:stable-slim'
    input:
    path text
    output:
    path "count.txt"
    script:
    """
    wc -c < ${text} > count.txt
    echo "counted ${params.note}" >&2
    """
}

params.note = 'first'

workflow {
    COUNT(UPPER(channel.of(file('a.txt'))))
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

/// The runs of the database, the latest first.
fn runs(directory: &Path) -> Vec<String> {
    let (code, output) = iunctus(directory, &["runs"]);
    assert_eq!(code, 0, "{output}");
    let lines = output.lines().skip(1);
    lines
        .filter_map(|line| line.split_whitespace().next().map(str::to_owned))
        .collect()
}

fn workflow(directory: &Path) {
    std::fs::write(directory.join("main.nf"), SCRIPT).expect("writes the workflow");
    std::fs::write(directory.join("a.txt"), "some text\n").expect("writes the input");
}

#[test]
fn simulated_runs_tell_where_the_time_went_and_what_differs() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path();
    workflow(path);
    let (code, output) = iunctus(path, &["run", "main.nf"]);
    assert_eq!(code, 0, "{output}");
    let (code, output) = iunctus(path, &["run", "main.nf", "--note", "second"]);
    assert_eq!(code, 0, "{output}");
    let [second, first] = runs(path).try_into().expect("two runs");

    let (code, output) = iunctus(path, &["time", &second]);
    assert_eq!(code, 0, "{output}");
    assert!(
        output.contains("the critical path, the chain of tasks"),
        "{output}"
    );
    assert!(output.contains("task 2 COUNT"), "{output}");
    assert!(
        output.contains("the times are those of the simulator"),
        "{output}"
    );
    let (code, output) = iunctus(path, &["time", &second, "-format", "json"]);
    assert_eq!(code, 0, "{output}");
    let time: serde_json::Value = serde_json::from_str(&output).expect("JSON");
    assert_eq!(time["critical_path"].as_array().map(Vec::len), Some(2));

    let (code, output) = iunctus(path, &["compare", &second]);
    assert_eq!(code, 0, "{output}");
    assert!(
        output.contains(&format!("comparing run {first}")),
        "{output}"
    );
    assert!(
        output.contains("the parameter `note` is not given in the first run and is `second`"),
        "{output}"
    );
    assert!(output.contains("1 the same, 1 changed"), "{output}");
    let (code, output) = iunctus(path, &["compare", &first, "no-run"]);
    assert_eq!(code, 1, "{output}");
    assert!(output.contains("has no run `no-run`"), "{output}");

    let (code, output) = iunctus(path, &["replay", &second, "2"]);
    assert_eq!(code, 1, "{output}");
    assert!(output.contains("was simulated"), "{output}");
}

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn a_task_replays_in_its_container() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path();
    workflow(path);
    let (code, output) = iunctus(path, &["run", "main.nf", "-executor", "local"]);
    assert_eq!(code, 0, "{output}");
    let [run] = runs(path).try_into().expect("one run");
    // The file that COUNT read is gone from its place; the store still has it by its digest.
    let stored = path.join(format!(
        "cache/iunctus/store/runs/{run}/UPPER-1/1/work/upper.txt"
    ));
    std::fs::remove_file(&stored).expect("removes the stored output");

    let (code, output) = iunctus(path, &["replay", &run, "2"]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("[done] COUNT"), "{output}");
    assert!(output.contains("  exit code: 0 before, 0 now"), "{output}");
    assert!(
        output.contains("every output has the same content as before"),
        "{output}"
    );
    assert!(
        output.contains("standard error now:\n  counted first"),
        "{output}"
    );
    let (code, output) = iunctus(path, &["runs"]);
    assert_eq!(code, 0, "{output}");
    assert!(
        output.contains(&format!("(replays task 2 of run {run})")),
        "{output}"
    );
}
