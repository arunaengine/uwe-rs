//! Checks failed-run draining and continued execution of independent jobs with keep-going.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{
    Attempt, Engine, Event, Executor, Launch, Observer, Order, Outcome, OutputFile, RunOptions,
    Running,
};

/// Snakemake 9.27.0 with `--cores 2` runs `fail` and `slow` at once. When `fail` has failed,
/// `slow` ends and `b.txt` stays, but `later` never starts; with `--keep-going`, `later` runs
/// and `c.txt` is made. Either way Snakemake exits with 1.
const SNAKEFILE: &str = r#"
rule all:
    input: "a.txt", "b.txt", "c.txt"

rule fail:
    output: "a.txt"
    shell: "sleep 1; exit 3"

rule slow:
    output: "b.txt"
    shell: "sleep 5; echo b > {output}"

rule later:
    input: "b.txt"
    output: "c.txt"
    shell: "cat {input} > {output}"
"#;

/// Fails the job of rule `fail`, makes the outputs of the others in a folder of its own, and
/// notes the rule of each job that started.
struct Scripted {
    folder: std::path::PathBuf,
    ran: Vec<String>,
}

impl Executor for Scripted {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        self.ran.push(task.step.clone());
        if task.step == "fail" {
            return Running::ended(Ok(Outcome {
                id: task.id,
                exit_code: 3,
                ..Outcome::default()
            }));
        }
        let mut files = Vec::new();
        for output in &task.outputs {
            let location = self.folder.join(format!("{}-{output}", task.id));
            std::fs::write(&location, "x\n").expect("writes");
            files.push(OutputFile {
                path: output.clone(),
                location: location.to_string_lossy().into_owned(),
                ..OutputFile::default()
            });
        }
        Running::ended(Ok(Outcome {
            id: task.id,
            files,
            ..Outcome::default()
        }))
    }
}

/// Notes the file names of the deliveries.
#[derive(Default)]
struct Delivered(Vec<String>);

impl Observer for Delivered {
    fn event(&mut self, event: Event<'_>) {
        if let Event::Delivered { delivery } = event {
            let name = std::path::Path::new(&delivery.target).file_name();
            self.0
                .push(name.unwrap_or_default().to_string_lossy().into_owned());
        }
    }
}

/// Runs the workflow with two jobs pending at once; the result has the error, the rules that
/// ran and the files delivered to the working directory.
fn run(keep_going: bool) -> (String, Vec<String>, Vec<String>) {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let workdir = folder.path().join("workflow");
    std::fs::create_dir(&workdir).expect("makes the folder");
    std::fs::write(workdir.join("Snakefile"), SNAKEFILE).expect("writes the Snakefile");
    let launch = Launch {
        target: workdir,
        scratch: folder.path().join("scratch"),
        keep_going,
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let made = folder.path().join("made");
    std::fs::create_dir(&made).expect("makes the folder");
    let mut executor = Scripted {
        folder: made,
        ran: Vec::new(),
    };
    let mut delivered = Delivered::default();
    let options = RunOptions {
        max_pending: 2,
        order: Order::AsAsked,
    };
    let execution = Engine::new(&mut executor)
        .options(options)
        .observer(&mut delivered)
        .run("Snakefile", driver.as_mut());
    let error = execution.error.map(|error| error.to_string());
    (error.expect("the run fails"), executor.ran, delivered.0)
}

#[test]
fn drains_failed_runs() {
    let (error, ran, delivered) = run(false);
    assert_eq!(error, "Error in rule fail: the task ended with exit code 3");
    assert_eq!(ran, ["fail", "slow"]);
    assert_eq!(delivered, ["b.txt"]);
}

#[test]
fn continues_independent_jobs() {
    let (error, ran, delivered) = run(true);
    assert_eq!(error, "Error in rule fail: the task ended with exit code 3");
    assert_eq!(ran, ["fail", "slow", "later"]);
    assert_eq!(delivered, ["b.txt", "c.txt"]);
}
