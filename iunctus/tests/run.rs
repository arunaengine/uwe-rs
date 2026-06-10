//! Runs `iunctus run` on small scripts with the mock executor, and with Docker in the ignored
//! test, which needs the `debian:stable-slim` image; run it with
//! `cargo test -p iunctus --test run -- --ignored`.
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
    let arguments = ["run", "main.nf", "-executor", "local"];
    let (code, output) = iunctus(directory.path(), &arguments);
    assert_eq!(code, 0, "{output}");
    // The three `view` operators print in an order that Nextflow does not define either.
    let mut lines: Vec<&str> = output.lines().collect();
    lines.sort_unstable();
    assert_eq!(
        lines,
        ["", "2", "TWO", "[done] UPPER (1)", "a: ONE", "shouted"]
    );
    let (code, output) = iunctus(directory.path(), &arguments);
    assert_eq!(code, 0, "{output}");
    assert!(output.starts_with("[cached] UPPER (1)\n"), "{output}");
}

#[test]
fn real_runs_need_an_image_for_each_task() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let script = "process P {\n    script:\n    'true'\n}\nworkflow {\n    P()\n}\n";
    std::fs::write(directory.path().join("main.nf"), script).expect("writes the script");
    // The check comes before the container engine, so this test needs no Docker.
    let (code, output) = iunctus(directory.path(), &["run", "main.nf", "-executor", "local"]);
    assert_eq!(code, 1, "{output}");
    assert!(
        output.contains("task P: the task names no container image"),
        "{output}"
    );
}

#[test]
fn a_bare_script_name_has_the_launch_directory_as_project() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(
        directory.path().join("main.nf"),
        "workflow {\n    println(projectDir == launchDir)\n}\n",
    )
    .expect("writes the script");
    let (code, output) = iunctus(directory.path(), &["run", "main.nf"]);
    assert_eq!((code, output.as_str()), (0, "true\n"));
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
    // DSL1 scripts are not supported (decision 14).
    std::fs::write(
        directory.path().join("dsl1.nf"),
        "process P {\n    input:\n    val x from Channel.from(1)\n    script:\n    'true'\n}\n",
    )
    .expect("writes the script");
    let (code, output) = iunctus(directory.path(), &["run", "dsl1.nf"]);
    assert_eq!(code, 33, "{output}");
    assert!(
        output.contains("dsl1.nf:1:1: DSL1 scripts are not supported (decision 14)"),
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

#[test]
fn mock_outputs_include_directories() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let script = "process P {\n    output:\n    path 'db/', emit: db\n    path 'db/x.txt', emit: x\n    \
                  path '*.{csv,tsv}', emit: table\n    script:\n    'tool'\n}\n\
                  workflow {\n    P()\n    P.out.db.view { d -> \"${d.name} ${d.isDirectory()}\" }\n    \
                  P.out.x.view { f -> f.name }\n    P.out.table.view { f -> f.name }\n}\n";
    std::fs::write(directory.path().join("main.nf"), script).expect("writes the script");
    let (code, output) = iunctus(directory.path(), &["run", "main.nf", "-executor", "mock"]);
    assert_eq!(code, 0, "{output}");
    let mut lines: Vec<&str> = output.lines().collect();
    lines.sort_unstable();
    assert_eq!(lines, ["[mock] P", "db true", "mock.csv", "x.txt"]);
}

#[test]
fn mock_outputs_are_the_names_in_the_script() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let script = "process P {\n    input:\n    val x\n    output:\n    path '*.fq', emit: reads\n    \
                  path x, emit: db\n    path '*cluster', emit: c\n    \
                  script:\n    'true'\n    \
                  stub:\n    \"touch ${x}_1.fq ${x}_2.fq\\nmkdir -p ${x}\\ntouch ${x}/a.txt\\n\
                  echo 'cluster' > ${x}.cluster\"\n}\n\
                  workflow {\n    P('s')\n    P.out.reads.view { f -> f*.name }\n    \
                  P.out.db.view { d -> \"${d.name} ${d.isDirectory()} ${d.list()}\" }\n    \
                  P.out.c.view { f -> f.name }\n}\n";
    std::fs::write(directory.path().join("main.nf"), script).expect("writes the script");
    let arguments = ["run", "main.nf", "-stub-run", "-executor", "mock"];
    let (code, output) = iunctus(directory.path(), &arguments);
    assert_eq!(code, 0, "{output}");
    let mut lines: Vec<&str> = output.lines().collect();
    lines.sort_unstable();
    assert_eq!(
        lines,
        [
            "[mock] P",
            "[s_1.fq, s_2.fq]",
            "s true [a.txt]",
            "s.cluster"
        ]
    );
    // An output that the stub surely does not make fails the task, as in Nextflow.
    let script = script.replace("path '*cluster', emit: c", "path 'none.txt', emit: c");
    std::fs::write(directory.path().join("main.nf"), script).expect("writes the script");
    let (code, output) = iunctus(directory.path(), &arguments);
    assert_eq!(code, 1, "{output}");
    assert!(
        output.contains("Missing output file(s) `none.txt` expected by process `P`"),
        "{output}"
    );
}
