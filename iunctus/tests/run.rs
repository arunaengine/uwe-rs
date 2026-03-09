//! Runs `iunctus run` on small scripts with Docker. Needs Docker and the `debian:stable-slim`
//! image; run with `cargo test -p iunctus --test run -- --ignored`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::process::Command;

const SCRIPT: &str = r#"
process UPPER {
    container 'debian:stable-slim'
    input:
    tuple val(id), path(text)
    output:
    tuple val(id), path("${id}.upper.txt"), emit: upper
    env('LINES'), emit: lines
    stdout emit: said
    script:
    """
    tr a-z A-Z < ${text} > ${id}.upper.txt
    LINES=\$(wc -l < ${text})
    echo shouted
    """
}

workflow {
    UPPER(channel.of(['a', file('input.txt')]))
    UPPER.out.upper.map { id, file -> "${id}: ${file.text.trim()}" }.view()
    UPPER.out.lines.view()
    UPPER.out.said.view()
}
"#;

/// Runs the binary in `directory`; the result has the exit code and the standard output.
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
#[ignore = "needs Docker and the debian:stable-slim image"]
fn runs_and_reuses_tasks() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(directory.path().join("main.nf"), SCRIPT).expect("writes the script");
    std::fs::write(directory.path().join("input.txt"), "one\ntwo\n").expect("writes the input");
    let (code, output) = iunctus(directory.path(), &["run", "main.nf"]);
    assert_eq!(code, 0, "{output}");
    // The three `view` operators print in an order that Nextflow does not define either.
    let mut lines: Vec<&str> = output.lines().collect();
    lines.sort_unstable();
    assert_eq!(
        lines,
        ["", "2", "TWO", "[done] UPPER (1)", "a: ONE", "shouted"]
    );
    let (code, output) = iunctus(directory.path(), &["run", "main.nf"]);
    assert_eq!(code, 0, "{output}");
    assert!(output.starts_with("[cached] UPPER (1)\n"), "{output}");
}

#[test]
fn unsupported_features_exit_with_33() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(
        directory.path().join("main.nf"),
        "enum Color {\n    RED\n}\n",
    )
    .expect("writes the script");
    let (code, output) = iunctus(directory.path(), &["run", "main.nf"]);
    assert_eq!(code, 33, "{output}");
    assert!(
        output.contains("main.nf:1:1: This script item is not supported yet"),
        "{output}"
    );
}

const GREETER: &str = r#"
process SAY {
    input:
    val(name)
    output:
    path("${name}.txt")
    script:
    """
    echo ${params.greeting} > ${name}.txt
    """
}

workflow {
    SAY(channel.of('a', 'b')).map { file -> "${file.name}: ${params.greeting}" }.view()
}
"#;

const GREETER_CONFIG: &str =
    "params.greeting = 'hi'\nprofiles {\n    loud { params.greeting = 'HI' }\n}\n";

#[test]
fn mock_runs_use_the_config_and_the_command_line() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(directory.path().join("main.nf"), GREETER).expect("writes the script");
    std::fs::write(directory.path().join("nextflow.config"), GREETER_CONFIG)
        .expect("writes the config");
    let arguments = ["run", "main.nf", "-executor", "mock", "-profile", "loud"];
    let (code, output) = iunctus(directory.path(), &arguments);
    assert_eq!(code, 0, "{output}");
    let mut lines: Vec<&str> = output.lines().collect();
    lines.sort_unstable();
    assert_eq!(
        lines,
        ["[mock] SAY (1)", "[mock] SAY (2)", "a.txt: HI", "b.txt: HI"]
    );
    let arguments = ["config", "-profile", "loud", "--greeting", "hey"];
    let (code, output) = iunctus(directory.path(), &arguments);
    assert_eq!(code, 0, "{output}");
    assert_eq!(output, "params.greeting = 'hey'\n");
}
