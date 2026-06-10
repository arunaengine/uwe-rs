//! A toy driver runs through the engine and the mock TES executor, and the graph links tasks by
//! the files they pass. The mocks make the files that a task's script names.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::mock::{EMPTY_GZIP, file_name, glob_matches};
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
    // The name of a placeholder file matches its pattern, which for `**/` needs a directory.
    assert_eq!(file_name("**/*.gz"), "mock/mock.gz");
    assert!(glob_matches("**/*.gz", &file_name("**/*.gz")));
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
    // Outputs that the script does not tell how it makes are made up.
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
    assert!(outcome.files[2].directory);
    let read = |name: &str| std::fs::read(work.join(name)).expect("reads a file");
    assert_eq!(read("versions.yml"), b"\"NFCORE:TOOL\":\n    mock: 1.0.0\n");
    // Inputs with content are written, as a TES server places them.
    assert_eq!(read(".command.sh"), b"true\n");
    assert!(!work.join("in.txt").exists());
    // A tool may make any output, which then is one named after the pattern.
    let task = Task {
        id: TaskId(8),
        script: Some("tool --out result\ntouch s_1.fq".into()),
        ..task
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
            "db",
            "copy.txt",
            "versions.yml",
            "mock.vcf.gz",
            "report"
        ]
    );
    let work = directory.path().join("8");
    assert_eq!(
        std::fs::read(work.join("mock.vcf.gz")).expect("reads"),
        EMPTY_GZIP
    );
    assert!(outcome.files[5].directory);
}

#[test]
fn mocks_write_the_text_of_stub_scripts() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let script = "cat <<-END > s.flagstat\n900 + 0 mapped (90.00% : N/A)\nEND\n\
                  touch s.lf.{drp,drs}\necho '' | gzip > s.fq.gz\ntouch versions.yml";
    let task = Task {
        id: TaskId(9),
        script: Some(script.into()),
        outputs: [
            "*.flagstat",
            "s.lf.*",
            "*.fq.gz",
            "*.unaligned.fasta",
            "versions.yml",
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
            "s.flagstat",
            "s.lf.drp",
            "s.lf.drs",
            "s.fq.gz",
            "mock.unaligned.fasta",
            "versions.yml"
        ]
    );
    let work = directory.path().join("9");
    let read = |name: &str| std::fs::read(work.join(name)).expect("reads a file");
    assert_eq!(read("s.flagstat"), b"900 + 0 mapped (90.00% : N/A)\n");
    // A versions file that the stub touches stays empty, as in Nextflow.
    assert_eq!(read("versions.yml"), b"");
    let mut text = String::new();
    std::io::Read::read_to_string(
        &mut flate2::read::GzDecoder::new(read("s.fq.gz").as_slice()),
        &mut text,
    )
    .expect("a gzip file");
    assert_eq!(text, "\n");
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

/// A script that surely fails, like one that gzips a file onto its input, ends so: the mock
/// gives the exit status and message of the command, and no files.
#[test]
fn mocks_of_scripts_that_fail_give_the_failure() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let task = Task {
        id: TaskId(4),
        script: Some("touch s.fastq\ngzip s.fastq\ntouch s.log".into()),
        inputs: vec![Input {
            path: "s.fastq.gz".into(),
            source: "/data/s.fastq.gz".into(),
            directory: false,
            content: None,
        }],
        outputs: ["*.fastq.gz", "*.log"].map(String::from).to_vec(),
        ..Task::default()
    };
    let outcome = Mock::new(directory.path())
        .execute(&task)
        .expect("an outcome");
    assert_eq!(outcome.exit_code, 2);
    assert_eq!(
        outcome.stderr,
        "gzip: s.fastq.gz already exists;\tnot overwritten"
    );
    assert!(outcome.files.is_empty());
}

/// An output of a tool that the simulation does not know is named after the arguments of the
/// tool that match its pattern, as the tool would name it, and is a directory when the pattern
/// ends with `/`. A folder argument like `res/` names no file.
#[test]
fn mocks_name_the_outputs_of_tools_after_their_arguments() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let task = Task {
        id: TaskId(5),
        script: Some(
            "mkdir output\nFeatureFinderMetabo -in s.mzML -out output/s.featureXML -oc report \
             -d res/"
                .into(),
        ),
        inputs: vec![Input {
            path: "s.mzML".into(),
            source: "/data/s.mzML".into(),
            directory: false,
            content: None,
        }],
        outputs: ["output/*.featureXML", "*.log", "report/", "res/*"]
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
        ["output/s.featureXML", "mock.log", "report", "res/mock"]
    );
    assert!(outcome.files[2].directory);
}
