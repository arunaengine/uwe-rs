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
