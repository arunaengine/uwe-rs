//! Connected same-image group jobs share one task, order their commands, and retry together.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{Launch, Outcome, Simulator, execute};

const WORKFLOW: &str = r#"
rule all:
    input: 'a.end', 'b.end'
rule first:
    output: '{sample}.start'
    group: 'work'
    container: 'debian:stable-slim'
    shell: 'echo {wildcards.sample} > {output}'
rule last:
    input: '{sample}.start'
    output: '{sample}.end'
    group: lambda wildcards: 'work'
    container: 'debian:stable-slim'
    retries: 1
    shell: 'cat {input} > {output}'
"#;

#[test]
fn groups_connected_jobs() {
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(folder.path().join("Snakefile"), WORKFLOW).unwrap();
    let launch = Launch {
        target: folder.path().into(),
        scratch: folder.path().join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).unwrap();
    let mut executor = Simulator::new(folder.path().join("made"));
    let run = execute("group", driver.as_mut(), &mut executor);
    assert!(run.error.is_none(), "{:?}", run.error);
    assert_eq!(run.graph.nodes.len(), 3);
    for node in &run.graph.nodes[..2] {
        assert_eq!(node.task.outputs.len(), 2);
        assert!(
            node.task
                .inputs
                .iter()
                .all(|input| !input.path.ends_with(".start"))
        );
        let script = node.task.script.as_ref().unwrap();
        assert!(script.find("echo").unwrap() < script.find("cat").unwrap());
    }
}

#[test]
fn splits_different_images() {
    let folder = tempfile::tempdir().unwrap();
    let workflow = WORKFLOW.replacen("container: 'debian:stable-slim'", "container: 'other'", 1);
    std::fs::write(folder.path().join("Snakefile"), workflow).unwrap();
    let launch = Launch {
        target: folder.path().into(),
        scratch: folder.path().join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).unwrap();
    let mut executor = Simulator::new(folder.path().join("made"));
    let run = execute("group", driver.as_mut(), &mut executor);
    assert!(run.error.is_none(), "{:?}", run.error);
    assert_eq!(run.graph.nodes.len(), 5);
}

#[test]
fn retries_whole_group() {
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(folder.path().join("Snakefile"), WORKFLOW).unwrap();
    let launch = Launch {
        target: folder.path().into(),
        scratch: folder.path().join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).unwrap();
    let first = driver.start().unwrap();
    assert_eq!(first.len(), 2);
    let again = driver
        .complete(Outcome {
            id: first[0].id,
            attempt: 1,
            exit_code: 1,
            ..Outcome::default()
        })
        .unwrap();
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].id, first[0].id);
    assert_eq!(again[0].outputs, first[0].outputs);
    assert_eq!(again[0].reuse, workflow_graph::Reuse::Denied);
}

#[test]
fn waits_external_inputs() {
    let folder = tempfile::tempdir().unwrap();
    let workflow = WORKFLOW.replace(
        "input: '{sample}.start'",
        "input: '{sample}.start', 'outside'",
    );
    std::fs::write(
        folder.path().join("Snakefile"),
        format!("{workflow}\nrule external:\n    output: 'outside'\n    shell: 'touch outside'\n"),
    )
    .unwrap();
    let launch = Launch {
        target: folder.path().into(),
        scratch: folder.path().join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).unwrap();
    let first = driver.start().unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].step, "external");
    let groups = driver
        .complete(Outcome {
            id: first[0].id,
            ..Outcome::default()
        })
        .unwrap();
    assert_eq!(groups.len(), 2);
}
