//! Every `iunctus run` records itself in the run database: `iunctus runs` lists it, and
//! `iunctus export` writes its RO-Crate. The runs use the simulator, so they need no container engine.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::process::Command;

const SCRIPT: &str = r#"
process UPPER {
    input:
    tuple val(id), path(text)
    output:
    tuple val(id), path("${id}.upper.txt")
    script:
    """
    tr a-z A-Z < ${text} > ${id}.upper.txt
    """
}

process COUNT {
    input:
    tuple val(id), path(text)
    output:
    path("${id}.count.txt")
    script:
    """
    wc -l < ${text} > ${id}.count.txt
    """
}

workflow {
    UPPER(channel.of(['a', file('a.txt')], ['b', file('b.txt')]))
    COUNT(UPPER.out)
}
"#;

/// Runs the binary in `directory`; the result has the exit code and the standard output and
/// error.
fn iunctus(directory: &Path, arguments: &[&str]) -> (i32, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_iunctus"))
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

#[test]
fn runs_are_recorded_and_exported() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path();
    std::fs::write(path.join("main.nf"), SCRIPT).expect("writes the script");
    std::fs::write(path.join("a.txt"), "one\ntwo\n").expect("writes an input");
    std::fs::write(path.join("b.txt"), "three\n").expect("writes an input");

    let (code, output) = iunctus(path, &["run", "main.nf", "-resume"]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("no earlier run"), "{output}");
    let (code, output) = iunctus(path, &["runs"]);
    assert_eq!(code, 0, "{output}");
    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(lines.len(), 2, "{output}");
    let first = lines[1]
        .split_whitespace()
        .next()
        .expect("a run id")
        .to_owned();
    assert!(
        lines[1].contains("succeeded") && lines[1].contains("simulated"),
        "{output}"
    );

    let (code, output) = iunctus(path, &["runs"]);
    assert_eq!(code, 0, "{output}");
    let last = output
        .lines()
        .nth(1)
        .and_then(|line| line.split_whitespace().next());
    let last = last.expect("a run id").to_owned();
    let (code, output) = iunctus(path, &["export", &last, "-output", "crate"]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("4 tasks"), "{output}");
    let metadata = std::fs::read_to_string(path.join("crate/ro-crate-metadata.json"))
        .expect("reads the metadata");
    let metadata: serde_json::Value = serde_json::from_str(&metadata).expect("JSON");
    let graph = metadata["@graph"].as_array().expect("a graph");
    let actions = graph
        .iter()
        .filter(|entity| entity["@type"] == "CreateAction")
        .count();
    // Four tasks and the workflow run.
    assert_eq!(actions, 5);
    assert!(path.join("crate/main.nf").is_file());

    let (code, output) = iunctus(path, &["export", "no-such-run"]);
    assert_eq!(code, 1, "{output}");
    assert!(output.contains("no run `no-such-run`"), "{output}");
}

#[test]
fn a_real_run_cannot_resume_a_simulated_one() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path();
    std::fs::write(path.join("main.nf"), SCRIPT).expect("writes the script");
    std::fs::write(path.join("a.txt"), "one\n").expect("writes an input");
    std::fs::write(path.join("b.txt"), "two\n").expect("writes an input");
    let (code, output) = iunctus(path, &["run", "main.nf"]);
    assert_eq!(code, 0, "{output}");
    let (_, runs) = iunctus(path, &["runs"]);
    let id = runs
        .lines()
        .nth(1)
        .and_then(|line| line.split_whitespace().next());
    let arguments = [
        "run",
        "main.nf",
        "-executor",
        "local",
        "-resume",
        id.expect("a run"),
    ];
    let (code, output) = iunctus(path, &arguments);
    assert_eq!(code, 1, "{output}");
    assert!(output.contains("was simulated"), "{output}");
}
