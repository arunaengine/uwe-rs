//! Target controls select jobs and forced jobs bypass task caches and resume history.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{Engine, Event, Launch, Observer, Reuse, Simulator};

#[derive(Default)]
struct Delivered(Vec<String>);

impl Observer for Delivered {
    fn event(&mut self, event: Event<'_>) {
        if let Event::Delivered { delivery } = event {
            self.0.push(delivery.target.clone());
        }
    }
}

const WORKFLOW: &str = r#"
rule all:
    input: "b.x", "b.y", "side"
rule seed:
    output: "a.{sample}"
    shell: "touch {output}"
rule branch:
    input: "a.{sample}"
    output: "b.{sample}"
    shell: "cp {input} {output}"
rule side:
    output: "side"
    shell: "touch {output}"
"#;

#[test]
fn forced_jobs_bypass_reuse_and_selected_temporary_files_are_delivered() {
    for forceall in [false, true] {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(
            folder.path().join("Snakefile"),
            WORKFLOW.replace("\"b.{sample}\"", "temp(\"b.{sample}\")"),
        )
        .unwrap();
        for output in ["a.x", "a.y", "b.x", "b.y", "side"] {
            std::fs::write(folder.path().join(output), "current").unwrap();
        }
        let launch = Launch {
            target: folder.path().into(),
            scratch: folder.path().join("scratch"),
            until: vec!["b.x".into()],
            forcerun: vec!["b.x".into()],
            forceall,
            simulated: true,
            ..Launch::default()
        };
        let mut driver = snakemake_runtime::driver(&launch).unwrap();
        let mut executor = Simulator::new(folder.path().join("simulator"));
        let mut delivered = Delivered::default();
        let result = Engine::new(&mut executor)
            .observer(&mut delivered)
            .run("test", driver.as_mut());
        assert!(result.error.is_none(), "{:?}", result.error);
        assert_eq!(result.graph.nodes.len(), if forceall { 2 } else { 1 });
        assert!(
            result
                .graph
                .nodes
                .iter()
                .all(|node| node.task.reuse == Reuse::Denied)
        );
        assert!(delivered.0.iter().any(|target| target.ends_with("b.x")));
    }
}

#[test]
fn a_forced_checkpoint_replans_from_new_outputs_and_respects_until() {
    use workflow_graph::{Outcome, OutputFile};
    for until in [false, true] {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(
            folder.path().join("Snakefile"),
            r#"
def selected(wildcards):
    with open(checkpoints.pick.get().output[0]) as choice:
        return choice.read().strip() + ".txt"
rule aggregate:
    input: selected
    output: "aggregate.txt"
    shell: "cp {input} {output}"
checkpoint pick:
    output: "choice"
    shell: "echo new > {output}"
"#,
        )
        .unwrap();
        for (path, bytes) in [
            ("choice", "STALE"),
            ("aggregate.txt", "old"),
            ("new.txt", "new bytes"),
        ] {
            std::fs::write(folder.path().join(path), bytes).unwrap();
        }
        let launch = Launch {
            target: folder.path().into(),
            scratch: folder.path().join("scratch"),
            forcerun: vec!["pick".into()],
            until: if until {
                vec!["choice".into()]
            } else {
                Vec::new()
            },
            ..Launch::default()
        };
        let mut driver = snakemake_runtime::driver(&launch).unwrap();
        let tasks = driver.start().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].step, "pick");
        assert_eq!(tasks[0].reuse, Reuse::Denied);
        let made = folder.path().join("made-choice");
        std::fs::write(&made, "new").unwrap();
        let next = driver
            .complete(Outcome {
                id: tasks[0].id,
                files: vec![OutputFile {
                    path: "choice".into(),
                    location: made.to_string_lossy().into_owned(),
                    ..OutputFile::default()
                }],
                ..Outcome::default()
            })
            .unwrap();
        if until {
            assert!(next.is_empty());
            driver.finish().unwrap();
        } else {
            assert_eq!(next.len(), 1);
            assert_eq!(next[0].step, "aggregate");
            assert_eq!(next[0].reuse, Reuse::Denied);
            assert!(next[0].inputs.iter().any(|input| input.path == "new.txt"));
        }
    }
}

#[test]
fn a_service_group_with_a_forced_consumer_bypasses_reuse() {
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(
        folder.path().join("Snakefile"),
        r#"
rule consume:
    input: "socket"
    output: "out"
    shell: "cat {input} > {output}"
rule server:
    output: service("socket")
    shell: "touch {output}; sleep 1000"
"#,
    )
    .unwrap();
    std::fs::write(folder.path().join("out"), "current").unwrap();
    let launch = Launch {
        target: folder.path().into(),
        scratch: folder.path().join("scratch"),
        forcerun: vec!["consume".into()],
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).unwrap();
    let tasks = driver.start().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].step, "server");
    assert_eq!(tasks[0].outputs, ["out"]);
    assert_eq!(tasks[0].reuse, Reuse::Denied);
}
