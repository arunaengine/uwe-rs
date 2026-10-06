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

#[test]
fn accepts_numeric_ids() {
    for (first, last, count) in [
        ("0", "0", 3),
        ("0", "False", 3),
        ("0", "'0'", 5),
        ("'{sample}'", "'{sample}'", 5),
    ] {
        let folder = tempfile::tempdir().unwrap();
        let workflow = WORKFLOW
            .replace("group: 'work'", &format!("group: {first}"))
            .replace(
                "group: lambda wildcards: 'work'",
                &format!("group: lambda wildcards: {last}"),
            );
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
        assert_eq!(run.graph.nodes.len(), count, "{first} and {last}");
    }
}

#[test]
fn rejects_component_cycle() {
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(folder.path().join("Snakefile"), "rule all:\n    input: 'end'\nrule first:\n    output: 'start'\n    group: 'work'\n    shell: 'touch {output}'\nrule outside:\n    input: 'start'\n    output: 'outside'\n    shell: 'touch {output}'\nrule last:\n    input: 'start', 'outside'\n    output: 'end'\n    group: 'work'\n    shell: 'touch {output}'\n").unwrap();
    let launch = Launch {
        target: folder.path().into(),
        scratch: folder.path().join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let error = snakemake_runtime::driver(&launch)
        .unwrap()
        .start()
        .unwrap_err();
    assert!(!error.unsupported, "{error}");
    assert!(error.to_string().contains("leave and re-enter"), "{error}");
}

#[test]
fn separates_group_scripts() {
    let folder = tempfile::tempdir().unwrap();
    for name in ["first", "last"] {
        let script = folder.path().join(name);
        std::fs::create_dir(&script).unwrap();
        std::fs::write(script.join("wrapper.py"), format!("print('{name}')\n")).unwrap();
    }
    let workflow = WORKFLOW
        .replace(
            "shell: 'echo {wildcards.sample} > {output}'",
            "script: 'first/wrapper.py'",
        )
        .replace(
            "shell: 'cat {input} > {output}'",
            "script: 'last/wrapper.py'",
        );
    std::fs::write(folder.path().join("Snakefile"), workflow).unwrap();
    let launch = Launch {
        target: folder.path().into(),
        scratch: folder.path().join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let groups = snakemake_runtime::driver(&launch).unwrap().start().unwrap();
    assert_eq!(groups.len(), 2);
    for task in groups {
        let scripts: Vec<_> = task
            .inputs
            .iter()
            .filter(|input| input.path.starts_with(".snakemake/scripts/"))
            .collect();
        assert_eq!(scripts.len(), 2);
        assert_ne!(scripts[0].path, scripts[1].path);
        for script in scripts {
            assert!(task.command[2].contains(&script.path));
        }
    }
}

#[test]
fn groups_after_checkpoint() {
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(
        folder.path().join("Snakefile"),
        r#"
rule all:
    input: 'last'
rule seed:
    output: 'seed'
    group: 'work'
    shell: 'touch {output}'
checkpoint middle:
    input: 'seed'
    output: 'middle'
    shell: 'touch {output}'
def after(wildcards):
    return checkpoints.middle.get().output[0]
rule last:
    input: after, 'seed'
    output: 'last'
    group: 'work'
    shell: 'touch {output}'
"#,
    )
    .unwrap();
    let launch = Launch {
        target: folder.path().into(),
        scratch: folder.path().join("scratch"),
        simulated: true,
        forceall: true,
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).unwrap();
    let mut executor = Simulator::new(folder.path().join("made"));
    let run = execute("group", driver.as_mut(), &mut executor);
    assert!(run.error.is_none(), "{:?}", run.error);
    assert_eq!(run.graph.nodes.len(), 4);
}
