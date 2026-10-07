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

#[test]
fn reuses_missing_intermediates() {
    for final_exists in [true, false] {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(
            folder.path().join("Snakefile"),
            r#"
rule all:
    input: "final"
rule seed:
    output: temp("intermediate")
    shell: "touch {output}"
rule finish:
    input: "intermediate"
    output: "final"
    shell: "cp {input} {output}"
"#,
        )
        .unwrap();
        if final_exists {
            std::fs::write(folder.path().join("final"), "current").unwrap();
        }
        let launch = Launch {
            target: folder.path().into(),
            scratch: folder.path().join("scratch"),
            simulated: true,
            ..Launch::default()
        };
        let mut driver = snakemake_runtime::driver(&launch).unwrap();
        let mut executor = Simulator::new(folder.path().join("simulator"));
        let result = execute("intermediates", driver.as_mut(), &mut executor);
        assert!(result.error.is_none(), "{:?}", result.error);
        let rules: Vec<_> = result
            .graph
            .nodes
            .iter()
            .map(|node| node.task.step.as_str())
            .collect();
        assert_eq!(
            rules,
            if final_exists {
                vec![]
            } else {
                vec!["seed", "finish", "all"]
            }
        );
    }
}

#[test]
fn reuses_checkpoint_results() {
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(
        folder.path().join("Snakefile"),
        r#"
def chosen(wildcards):
    return checkpoints.seed.get().output[0]
rule all:
    input: "final"
checkpoint seed:
    output: "choice"
    shell: "touch {output}"
rule finish:
    input: chosen
    output: "final"
    shell: "cp {input} {output}"
"#,
    )
    .unwrap();
    std::fs::write(folder.path().join("final"), "current").unwrap();
    let launch = Launch {
        target: folder.path().into(),
        scratch: folder.path().join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).unwrap();
    let mut executor = Simulator::new(folder.path().join("simulator"));
    let result = execute("checkpoint", driver.as_mut(), &mut executor);
    assert!(result.error.is_none(), "{:?}", result.error);
    assert!(result.graph.nodes.is_empty());
}

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
fn reruns_newer_inputs() {
    // Snakemake 9.27 runs `copy` again for an input newer than its output, not `keep`.
    assert_eq!(steps(10, None), ["all", "copy"]);
    assert!(steps(1000, None).is_empty());
}

#[test]
fn reuses_identical_inputs() {
    // SHA-256 of "x\n", as Snakemake 9.27 records it for the input of `copy.txt`.
    let digest = "73cb3858a687a8494ca3323053016282f3dad39d42cf62ca4e79dda2aac7d9ac";
    let same = format!(r#"{{"input_checksums": {{"in.txt": "{digest}"}}}}"#);
    let other = r#"{"input_checksums": {"in.txt": "sha256:00"}}"#;
    assert!(steps(10, Some(&same)).is_empty());
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
fn copies_update_outputs() {
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
fn isolates_update_inputs() {
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

#[test]
fn reruns_changed_checkpoints() {
    use workflow_graph::{Driver, Files, LocalFiles, Outcome, OutputFile, Task};

    let folder = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(
        folder.path().join("Snakefile"),
        r#"
def selected(wildcards):
    with open(checkpoints.pick.get().output[0]) as manifest:
        return manifest.read().strip() + ".txt"
rule aggregate:
    input: selected
    output: "aggregate.txt"
    shell: "cat {input} > {output}"
checkpoint pick:
    input: "source.txt"
    output: "selection.txt"
    shell: "cp {input} {output}"
"#,
    )
    .expect("writes the Snakefile");
    for (path, content) in [
        ("source.txt", "A"),
        ("A.txt", "first\n"),
        ("B.txt", "second\n"),
    ] {
        std::fs::write(folder.path().join(path), content).expect("writes an input");
    }
    let launch = Launch {
        target: folder.path().to_owned(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let complete = |driver: &mut dyn Driver, task: &Task, bytes: &[u8]| {
        let made = folder.path().join(format!("made-{}", task.id));
        std::fs::write(&made, bytes).expect("writes task output");
        let next = driver
            .complete(Outcome {
                id: task.id,
                files: vec![OutputFile {
                    path: task.outputs[0].clone(),
                    location: made.to_string_lossy().into_owned(),
                    ..OutputFile::default()
                }],
                ..Outcome::default()
            })
            .expect("accepts the output");
        for delivery in driver.deliveries() {
            LocalFiles.deliver(&delivery).expect("delivers output");
            driver.delivered(&delivery).expect("acknowledges delivery");
        }
        next
    };
    let mut first = snakemake_runtime::driver(&launch).expect("a driver");
    let tasks = first.start().expect("starts the first run");
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].step, "pick");
    let tasks = complete(first.as_mut(), &tasks[0], b"A");
    assert_eq!(tasks.len(), 1);
    assert!(tasks[0].inputs.iter().any(|input| input.path == "A.txt"));
    assert!(complete(first.as_mut(), &tasks[0], b"first\n").is_empty());
    first.finish().expect("finishes the first run");

    let now = SystemTime::now();
    std::fs::write(folder.path().join("source.txt"), "B").expect("changes the checkpoint source");
    for (path, age) in [("selection.txt", 100), ("source.txt", 10)] {
        std::fs::File::options()
            .write(true)
            .open(folder.path().join(path))
            .expect("opens")
            .set_modified(now - Duration::from_secs(age))
            .expect("sets the time");
    }
    std::fs::remove_file(folder.path().join("aggregate.txt")).expect("removes only the aggregate");
    let mut second = snakemake_runtime::driver(&launch).expect("a driver");
    let tasks = second
        .start()
        .expect("plans without reading the stale manifest");
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].step, "pick");
    let checkpoint = tasks[0].id;
    let tasks = complete(second.as_mut(), &tasks[0], b"B");
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].step, "aggregate");
    assert!(tasks[0].after.contains(&checkpoint));
    let input = tasks[0]
        .inputs
        .iter()
        .find(|input| input.path == "B.txt")
        .expect("the selected input");
    let bytes = std::fs::read(&input.source).expect("reads selected bytes");
    assert_eq!(bytes, b"second\n");
    assert!(complete(second.as_mut(), &tasks[0], &bytes).is_empty());
    second.finish().expect("finishes the second run");
    assert_eq!(
        std::fs::read(folder.path().join("aggregate.txt")).expect("reads result"),
        b"second\n"
    );
    std::fs::remove_file(folder.path().join("aggregate.txt")).expect("removes the aggregate again");
    let mut third = snakemake_runtime::driver(&launch).expect("a driver");
    let tasks = third
        .start()
        .expect("uses the checkpoint that is up to date");
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].step, "aggregate");
    assert!(tasks[0].inputs.iter().any(|input| input.path == "B.txt"));
    assert!(complete(third.as_mut(), &tasks[0], b"second\n").is_empty());
    third.finish().expect("finishes the third run");
}
