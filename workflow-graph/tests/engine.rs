//! A toy driver runs through the engine and the mock TES executor, and the graph links tasks by
//! the files they pass.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::mock::glob_matches;
use workflow_graph::{
    Driver, DriverError, EdgeKind, Input, Language, MockTes, Outcome, Task, TaskId, execute,
};

/// Two `split` tasks, then one `merge` task that reads both outputs.
#[derive(Default)]
struct Toy {
    finished: Vec<Outcome>,
}

impl Driver for Toy {
    fn language(&self) -> Language {
        Language::Snakemake
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        Ok((1..=2)
            .map(|n| Task {
                id: TaskId(n),
                name: format!("split {n}"),
                step: "split".into(),
                image: Some("ubuntu:24.04".into()),
                command: vec![
                    "bash".into(),
                    "-c".into(),
                    format!("echo {n} > part{n}.txt"),
                ],
                script: Some(format!("echo {n} > part{n}.txt")),
                outputs: vec!["part*.txt".into()],
                ..Task::default()
            })
            .collect())
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        self.finished.push(outcome);
        if self.finished.len() != 2 {
            return Ok(Vec::new());
        }
        let inputs = self
            .finished
            .iter()
            .flat_map(|outcome| &outcome.files)
            .map(|file| Input {
                path: file.path.clone(),
                source: file.location.to_string_lossy().into_owned(),
                directory: false,
                content: None,
            })
            .collect();
        Ok(vec![Task {
            id: TaskId(3),
            name: "merge".into(),
            step: "merge".into(),
            command: vec!["cat".into()],
            inputs,
            outputs: vec!["merged/".into()],
            ..Task::default()
        }])
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

#[test]
fn the_graph_links_tasks_by_their_files() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let mut executor = MockTes::new(directory.path());
    let graph = execute("toy", &mut Toy::default(), &mut executor)
        .into_result()
        .expect("the run succeeds");
    assert_eq!(graph.nodes.len(), 3);
    assert_eq!(graph.steps(), ["split", "merge"]);
    graph.check().expect("a valid graph");
    let into_merge: Vec<_> = graph
        .edges
        .iter()
        .filter(|edge| edge.to == TaskId(3))
        .collect();
    assert_eq!(into_merge.len(), 2);
    assert_eq!(
        into_merge[0].kind,
        EdgeKind::File {
            path: "part1.txt".into()
        }
    );
    assert!(directory.path().join("work/1/part1.txt").is_file());
    assert!(directory.path().join("work/3/merged").is_dir());
    let tes: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(directory.path().join("tes/1.json")).expect("a TES document"),
    )
    .expect("JSON");
    assert_eq!(tes["executors"][0]["image"], "ubuntu:24.04");
    assert!(graph.to_dot().contains("t1 -> t3"));
}

#[test]
fn globs_match_like_a_shell() {
    assert!(glob_matches("*.txt", "a.txt"));
    assert!(!glob_matches("*.txt", "d/a.txt"));
    assert!(glob_matches("**/*.txt", "d/e/a.txt"));
    assert!(glob_matches("*.{html,zip}", "r.zip"));
    assert!(glob_matches("s[0-9].bam", "s3.bam"));
}
