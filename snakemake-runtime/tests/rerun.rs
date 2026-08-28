//! A job whose outputs exist runs again when an input is newer than its oldest output, as
//! Snakemake's default `mtime` trigger decides, unless the input is `ancient`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::{Duration, SystemTime};

use workflow_graph::{Launch, Simulator, Writable, execute};

const SNAKEFILE: &str = r#"
rule all:
    input: "copy.txt", "kept.txt"

rule copy:
    input: "in.txt"
    output: "copy.txt"
    shell: "cat {input} > {output}"

rule keep:
    input: ancient("in.txt")
    output: "kept.txt"
    shell: "cat {input} > {output}"
"#;

/// The steps that a simulated run of the folder runs, with Snakemake's record of `copy.txt`
/// when given.
fn steps(input_age: u64, record: Option<&str>) -> Vec<String> {
    let scratch = tempfile::tempdir().expect("a temporary directory");
    let folder = scratch.path().join("workflow");
    std::fs::create_dir_all(&folder).expect("makes the folder");
    std::fs::write(folder.join("Snakefile"), SNAKEFILE).expect("writes the Snakefile");
    let now = SystemTime::now();
    let write = |name: &str, age: u64| {
        let path = folder.join(name);
        std::fs::write(&path, "x\n").expect("writes");
        let file = std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("opens");
        file.set_modified(now - Duration::from_secs(age))
            .expect("sets the time");
    };
    write("copy.txt", 100);
    write("kept.txt", 100);
    write("in.txt", input_age);
    if let Some(record) = record {
        let folder = folder.join(".snakemake/metadata");
        std::fs::create_dir_all(&folder).expect("makes the folder");
        // The name of `copy.txt` in URL-safe Base64.
        std::fs::write(folder.join("Y29weS50eHQ="), record).expect("writes the record");
    }
    let launch = Launch {
        target: folder.clone(),
        scratch: scratch.path().to_path_buf(),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let mut executor = Simulator::new(scratch.path().join("simulator"));
    let execution = execute(&folder.to_string_lossy(), driver.as_mut(), &mut executor);
    assert!(execution.error.is_none(), "{:?}", execution.error);
    let mut steps: Vec<String> = (execution.graph.nodes.iter())
        .map(|node| node.task.step.clone())
        .collect();
    steps.sort();
    steps
}

#[test]
fn newer_inputs_run_their_jobs_again() {
    // Snakemake 9.27 runs `copy` again for an input newer than its output, not `keep`.
    assert_eq!(steps(10, None), ["all", "copy"]);
    assert_eq!(steps(1000, None), ["all"]);
}

#[test]
fn newer_inputs_with_the_recorded_checksum_do_not() {
    // SHA-256 of "x\n", as Snakemake 9.27 records it for the input of `copy.txt`.
    let digest = "73cb3858a687a8494ca3323053016282f3dad39d42cf62ca4e79dda2aac7d9ac";
    let same = format!(r#"{{"input_checksums": {{"in.txt": "{digest}"}}}}"#);
    let other = r#"{"input_checksums": {"in.txt": "sha256:00"}}"#;
    assert_eq!(steps(10, Some(&same)), ["all"]);
    assert_eq!(steps(10, Some(other)), ["all", "copy"]);
}

const UPDATE: &str = r#"
rule all:
    input: "out.txt", "test.txt"

rule b:
    input: before_update("test.txt")
    output: "out.txt"
    shell: "cp {input} {output}"

rule a:
    input: "in.txt"
    output: update("test.txt")
    shell: "echo bar >> test.txt"
"#;

#[test]
fn update_outputs_are_copies_and_before_update_inputs_need_no_job() {
    let scratch = tempfile::tempdir().expect("a temporary directory");
    let folder = scratch.path().join("workflow");
    std::fs::create_dir_all(&folder).expect("makes the folder");
    std::fs::write(folder.join("Snakefile"), UPDATE).expect("writes the Snakefile");
    let now = SystemTime::now();
    for (name, age) in [("test.txt", 100), ("in.txt", 10)] {
        let path = folder.join(name);
        std::fs::write(&path, "x\n").expect("writes");
        let file = std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("opens");
        file.set_modified(now - Duration::from_secs(age))
            .expect("sets the time");
    }
    let launch = Launch {
        target: folder.clone(),
        scratch: scratch.path().to_path_buf(),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let mut executor = Simulator::new(scratch.path().join("simulator"));
    let execution = execute(&folder.to_string_lossy(), driver.as_mut(), &mut executor);
    assert!(execution.error.is_none(), "{:?}", execution.error);
    let task = |step: &str| {
        let node = execution
            .graph
            .nodes
            .iter()
            .find(|node| node.task.step == step);
        node.map(|node| node.task.clone())
            .expect("the task of the step")
    };
    // Snakemake 9.27 appends to test.txt, so `a` gets a copy of it to change (its
    // test_update_flag); `b` reads it without waiting for `a`.
    let a = task("a");
    let copy = a.inputs.iter().find(|input| input.path == "test.txt");
    assert_eq!(copy.map(|input| input.writable), Some(Writable::Copy));
    let b = execution
        .graph
        .nodes
        .iter()
        .find(|node| node.task.step == "b");
    let b = b.expect("the task of b");
    assert!(
        execution
            .graph
            .edges
            .iter()
            .all(|edge| edge.to != b.task.id),
        "{:?}",
        execution.graph.edges
    );
}

#[test]
fn before_update_inputs_with_the_same_basename_keep_distinct_bytes() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    for (path, content) in [("a/data.txt", "first\n"), ("b/data.txt", "second\n")] {
        let path = folder.path().join(path);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("makes folders");
        std::fs::write(path, content).expect("writes");
    }
    std::fs::write(
        folder.path().join("Snakefile"),
        r#"
rule copy:
    input: before_update("a/data.txt"), before_update("b/data.txt")
    output: "out.txt"
    shell: "cat {input} > {output}"
"#,
    )
    .expect("writes the Snakefile");
    let launch = Launch {
        target: folder.path().to_owned(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let tasks = driver.start().expect("starts");
    assert_eq!(tasks.len(), 1);
    let input = |path: &str| {
        tasks[0]
            .inputs
            .iter()
            .find(|input| input.path == path)
            .expect("an input")
    };
    let first = input("a/data.txt");
    let second = input("b/data.txt");
    assert_ne!(first.source, second.source);
    std::fs::write(folder.path().join("a/data.txt"), "changed\n").expect("updates the source");
    assert_eq!(
        std::fs::read(&first.source).expect("reads snapshot"),
        b"first\n"
    );
    assert_eq!(
        std::fs::read(&second.source).expect("reads snapshot"),
        b"second\n"
    );
}
