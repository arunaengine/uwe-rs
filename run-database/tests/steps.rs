//! Checks step comparisons across changed tasks, input content and output digests.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use run_database::{Database, RunStart, StepChange};
use workflow_graph::{Input, Language, Outcome, OutputFile, Task, TaskId};

fn start(id: &str) -> RunStart {
    RunStart {
        id: id.into(),
        resumes: None,
        workflow: "/work/main.nf".into(),
        language: Language::Nextflow,
        revision: None,
        arguments: Vec::new(),
        parameters: Vec::new(),
        executor: "simulator".into(),
        simulated: true,
        store: None,
        program: None,
        launch: None,
        workflow_folder: None,
        repeats: None,
    }
}

/// A task of a step that reads files at `sources` and writes `out.txt` with `made` in it.
struct Planned<'a> {
    id: u64,
    step: &'a str,
    script: &'a str,
    sources: Vec<String>,
    made: &'a str,
}

/// Records a run of tasks, each writing its output below `folder/<run>/<id>/`, and returns
/// where each task's output is.
fn record(path: &Path, folder: &Path, run: &str, tasks: &[Planned]) {
    let mut recorder = Database::open(path)
        .expect("opens")
        .record(&start(run))
        .expect("records");
    for planned in tasks {
        let task = Task {
            id: TaskId(planned.id),
            name: format!("{} ({})", planned.step, planned.id),
            step: planned.step.into(),
            script: Some(planned.script.into()),
            inputs: (planned.sources.iter().enumerate())
                .map(|(index, source)| Input {
                    path: format!("in{index}.txt"),
                    source: source.clone(),
                    ..Input::default()
                })
                .collect(),
            outputs: vec!["out.txt".into()],
            ..Task::default()
        };
        recorder.requested(&task, &[]);
        recorder.started(TaskId(planned.id), 1);
        let location = folder
            .join(run)
            .join(planned.id.to_string())
            .join("out.txt");
        std::fs::create_dir_all(location.parent().expect("a parent")).expect("makes a folder");
        std::fs::write(&location, planned.made).expect("writes");
        let outcome = Outcome {
            id: TaskId(planned.id),
            files: vec![OutputFile {
                path: "out.txt".into(),
                location: location.to_string_lossy().into_owned(),
                digest: Some(format!("sha256:{}", planned.made)),
                ..OutputFile::default()
            }],
            ..Outcome::default()
        };
        recorder.finished(&outcome, 1, false);
    }
    recorder.ended(None);
    recorder.close().expect("closes");
}

#[test]
fn compares_step_content() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let (path, folder) = (directory.path().join("runs.sqlite"), directory.path());
    let output = |run: &str, id: u64| {
        let location = folder.join(run).join(id.to_string()).join("out.txt");
        location.to_string_lossy().into_owned()
    };
    let raw = folder.join("raw.txt");
    std::fs::write(&raw, "raw").expect("writes");
    let raw = raw.to_string_lossy().into_owned();
    // MAKE is the same in both runs, though the ids of its tasks differ; JOIN lists its inputs
    // in the order they finished; HEAD reads what JOIN made; SPLIT has another number of
    // tasks; STAMP writes the time.
    for (run, first, second, order, stamp, split) in [
        ("first-run", 1, 2, "cat in0.txt in1.txt", "10:00", 1),
        ("second-run", 2, 1, "cat in1.txt in0.txt", "10:01", 2),
    ] {
        let mut tasks = vec![
            Planned {
                id: first,
                step: "MAKE",
                script: "echo 1",
                sources: vec![raw.clone()],
                made: "one",
            },
            Planned {
                id: second,
                step: "MAKE",
                script: "echo 2",
                sources: vec![raw.clone()],
                made: "two",
            },
        ];
        tasks.push(Planned {
            id: 3,
            step: "JOIN",
            script: order,
            sources: vec![output(run, 1), output(run, 2)],
            made: if run == "first-run" {
                "onetwo"
            } else {
                "twoone"
            },
        });
        tasks.push(Planned {
            id: 4,
            step: "HEAD",
            script: "head -c 3 in0.txt",
            sources: vec![output(run, 3)],
            made: if run == "first-run" { "one" } else { "two" },
        });
        for id in 0..split {
            tasks.push(Planned {
                id: 10 + id,
                step: "SPLIT",
                script: "split",
                sources: vec![raw.clone()],
                made: "part",
            });
        }
        tasks.push(Planned {
            id: 20,
            step: "STAMP",
            script: "date",
            sources: Vec::new(),
            made: stamp,
        });
        record(&path, folder, run, &tasks);
    }
    let database = Database::open(&path).expect("opens");
    let differences = database
        .compare_steps("first-run", "second-run")
        .expect("compares");
    let found: Vec<(&str, StepChange)> = (differences.iter())
        .map(|difference| (difference.step.as_str(), difference.change))
        .collect();
    assert_eq!(
        found,
        [
            ("JOIN", StepChange::Made),
            ("HEAD", StepChange::Inputs),
            ("SPLIT", StepChange::Tasks),
            ("STAMP", StepChange::Outputs),
        ]
    );
    assert_eq!(differences[0].outputs, ["out.txt"]);
    assert_eq!(differences[1].after, ["JOIN"]);
    assert_eq!(differences[2].tasks, (1, 2));
}
