//! A toy driver runs through the engine and the mock TES executor, and the graph links tasks by
//! the files they pass. The mocks make the files that a task's script names.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::mock::{EMPTY_GZIP, glob_matches};
use workflow_graph::{
    Driver, DriverError, EdgeKind, Executor, Input, Language, Mock, MockTes, Outcome, Task, TaskId,
    execute,
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

#[test]
fn mocks_make_the_files_that_the_script_names() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let script = "touch s_1.fq s_2.fq\nmkdir -p db\ntouch db/x.txt\ncp in.txt copy.txt\n\
                  echo v > versions.yml";
    let input = |path: &str, content: Option<&str>| Input {
        path: path.into(),
        source: format!("/data/{path}"),
        directory: false,
        content: content.map(str::to_owned),
    };
    let task = Task {
        id: TaskId(7),
        name: "TOOL (s)".into(),
        step: "NFCORE:TOOL".into(),
        script: Some(script.into()),
        inputs: vec![input(".command.sh", Some("true\n")), input("in.txt", None)],
        outputs: [
            "*.fq",
            "db",
            "copy.txt",
            "versions.yml",
            "*.vcf.gz",
            "report/",
        ]
        .map(String::from)
        .to_vec(),
        ..Task::default()
    };
    let outcome = Mock::new(directory.path())
        .execute(&task)
        .expect("an outcome");
    let paths: Vec<&str> = outcome
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(
        paths,
        [
            "s_1.fq",
            "s_2.fq",
            "db",
            "copy.txt",
            "versions.yml",
            "mock.vcf.gz",
            "report"
        ]
    );
    let work = directory.path().join("7");
    assert!(work.join("db/x.txt").is_file());
    assert!(outcome.files[2].directory && outcome.files[6].directory);
    let read = |name: &str| std::fs::read(work.join(name)).expect("reads a file");
    assert_eq!(read("versions.yml"), b"\"NFCORE:TOOL\":\n    mock: 1.0.0\n");
    assert_eq!(read("mock.vcf.gz"), EMPTY_GZIP);
    // Inputs with content are written, as a TES server places them.
    assert_eq!(read(".command.sh"), b"true\n");
    assert!(!work.join("in.txt").exists());
}

#[test]
fn mocks_of_commands_without_a_script_make_the_files_the_arguments_name() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let command = [
        "abra",
        "--in",
        "/in/a.bam",
        "--out",
        "a_IR.bam,b_IR.bam",
        "--log=run.log",
    ];
    let task = Task {
        id: TaskId(3),
        command: command.map(String::from).to_vec(),
        outputs: ["*_IR.bam", "*.log", "*.txt"].map(String::from).to_vec(),
        ..Task::default()
    };
    let outcome = Mock::new(directory.path())
        .execute(&task)
        .expect("an outcome");
    let paths: Vec<&str> = outcome
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(paths, ["a_IR.bam", "b_IR.bam", "run.log", "mock.txt"]);
}
