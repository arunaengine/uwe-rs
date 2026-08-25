//! Runs `iunctus run` on small scripts with the simulator, and with Docker in the ignored
//! test, which needs the `debian:stable-slim` image; run it with
//! `cargo test -p iunctus --test run -- --ignored`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::process::Command;

use workflow_graph::SIMULATION;

mod common;

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

/// Runs the binary in `directory`; the result has the exit code and the standard output and
/// error, without the note of a simulated run, which `tests/graph.rs` checks.
fn iunctus(directory: &Path, arguments: &[&str]) -> (i32, String) {
    let output = common::command()
        .args(arguments)
        .current_dir(directory)
        .env("XDG_CACHE_HOME", directory.join("cache"))
        .output()
        .expect("runs iunctus");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.replace(&format!("iunctus: {SIMULATION}\n"), "");
    (
        output.status.code().unwrap_or(-1),
        common::without_display(&format!("{stdout}{stderr}")),
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
#[ignore = "needs Docker and the debian:stable-slim image"]
fn a_real_run_names_itself_and_says_where_its_results_went() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let script = "process SAY {\n    container 'debian:stable-slim'\n    \
                  publishDir 'results', mode: 'copy'\n    input:\n    val x\n    \
                  output:\n    path \"${x}.txt\"\n    script:\n    \"echo ${x} > ${x}.txt\"\n}\n\
                  workflow {\n    SAY(channel.of('a', 'b'))\n}\n";
    std::fs::write(directory.path().join("main.nf"), script).expect("writes the script");
    let output = common::command()
        .args(["run", "main.nf", "-executor", "local"])
        .current_dir(directory.path())
        .env("XDG_CACHE_HOME", directory.path().join("cache"))
        .output()
        .expect("runs iunctus");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    // Standard output is no terminal here, so the lines come one after another.
    let lines: Vec<&str> = stdout.lines().collect();
    let header = format!(
        "iunctus {}: running main.nf (Nextflow) on local, run ",
        env!("CARGO_PKG_VERSION")
    );
    assert!(lines[0].starts_with(&header), "{stdout}");
    let run = &lines[0][header.len()..];
    let mut tasks = lines[1..3].to_vec();
    tasks.sort_unstable();
    assert_eq!(tasks, ["[done] SAY (1)", "[done] SAY (2)"]);
    // At the end, the steps with their counts, and the summary.
    assert!(lines[3].starts_with("executor >  local (2 tasks, 0 running, "));
    assert!(lines[4].ends_with(" | 2 of 2 \u{2714}"), "{stdout}");
    assert!(lines[6].starts_with("Done in "), "{stdout}");
    assert!(
        lines[6].ends_with(": 2 tasks, 2 ran, 0 from the cache, 0 failed"),
        "{stdout}"
    );
    assert_eq!(lines[7..9], ["Results:", "  results/ a.txt, b.txt"]);
    assert!(lines[9].starts_with("Files of every task: "), "{stdout}");
    let more = format!("More: `iunctus runs`, `iunctus page {run}`, `iunctus time {run}`");
    assert_eq!(lines[10], more);
}

/// Two processes that sleep for a minute each.
const SLEEPERS: &str = r#"
process SLEEP {
    container 'debian:stable-slim'
    input:
    val n
    script:
    """
    sleep 60
    """
}

workflow {
    SLEEP(channel.of(1, 2))
}
"#;

#[test]
#[ignore = "needs Docker and the debian:stable-slim image"]
fn ctrl_c_stops_the_running_tasks() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(directory.path().join("main.nf"), SLEEPERS).expect("writes the script");
    let mut child = common::command()
        .args(["run", "main.nf", "-executor", "local"])
        .current_dir(directory.path())
        .env("XDG_CACHE_HOME", directory.path().join("cache"))
        .spawn()
        .expect("runs iunctus");
    std::thread::sleep(std::time::Duration::from_secs(4));
    let begin = std::time::Instant::now();
    let interrupt = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("sends Ctrl-C");
    assert!(interrupt.success());
    let status = child.wait().expect("iunctus ends");
    assert_eq!(status.code(), Some(130));
    // Both containers stopped at once, long before their minute.
    assert!(begin.elapsed() < std::time::Duration::from_secs(30));
    let scratch = directory.path().join("cache/iunctus/scratch");
    let left = std::fs::read_dir(&scratch).map_or(0, |entries| entries.count());
    assert_eq!(
        left,
        0,
        "scratch directories are left in {}",
        scratch.display()
    );
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
    // Also when a DSL1 output line does not parse, as in OpenEBench's validation workflow.
    std::fs::write(
        directory.path().join("dsl1.nf"),
        "process P {\n    output:\n    val task.exitStatus into EXIT_STAT\n    script:\n    'true'\n}\n\
         process Q {\n    input:\n    val x from EXIT_STAT\n    script:\n    'true'\n}\n",
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
fn simulated_runs_use_the_config_and_the_command_line() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(directory.path().join("main.nf"), GREETER).expect("writes the script");
    std::fs::write(directory.path().join("nextflow.config"), GREETER_CONFIG)
        .expect("writes the config");
    let arguments = [
        "run",
        "main.nf",
        "-executor",
        "simulator",
        "-profile",
        "loud",
    ];
    let (code, output) = iunctus(directory.path(), &arguments);
    assert_eq!(code, 0, "{output}");
    let mut lines: Vec<&str> = output.lines().collect();
    lines.sort_unstable();
    assert_eq!(
        lines,
        [
            "[simulated] SAY (1)",
            "[simulated] SAY (2)",
            "a.txt: HI",
            "b.txt: HI"
        ]
    );
    let arguments = ["config", "-profile", "loud", "--greeting", "hey"];
    let (code, output) = iunctus(directory.path(), &arguments);
    assert_eq!(code, 0, "{output}");
    assert_eq!(output, "params.greeting = 'hey'\n");
}

#[test]
fn simulated_outputs_include_directories() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let script = "process P {\n    output:\n    path 'db/', emit: db\n    path 'db/x.txt', emit: x\n    \
                  path '*.{csv,tsv}', emit: table\n    script:\n    'tool'\n}\n\
                  workflow {\n    P()\n    P.out.db.view { d -> \"${d.name} ${d.isDirectory()}\" }\n    \
                  P.out.x.view { f -> f.name }\n    P.out.table.view { f -> f.name }\n}\n";
    std::fs::write(directory.path().join("main.nf"), script).expect("writes the script");
    let (code, output) = iunctus(
        directory.path(),
        &["run", "main.nf", "-executor", "simulator"],
    );
    assert_eq!(code, 0, "{output}");
    let mut lines: Vec<&str> = output.lines().collect();
    lines.sort_unstable();
    assert_eq!(
        lines,
        ["[simulated] P", "db true", "simulated.csv", "x.txt"]
    );
}

#[test]
fn simulated_outputs_are_the_names_in_the_script() {
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
    let arguments = ["run", "main.nf", "-stub-run", "-executor", "simulator"];
    let (code, output) = iunctus(directory.path(), &arguments);
    assert_eq!(code, 0, "{output}");
    let mut lines: Vec<&str> = output.lines().collect();
    lines.sort_unstable();
    assert_eq!(
        lines,
        [
            "[s_1.fq, s_2.fq]",
            "[simulated] P",
            // `d.list()` lists a made-up folder, which a real run may fill with other files.
            "iunctus: the graph marks 1 point where real data can add or remove tasks.",
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
