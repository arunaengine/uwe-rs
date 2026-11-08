//! Runs the jobs of Snakefiles one by one through the job adapter and compares their tasks,
//! cache keys, resume fingerprints, retries and acceptance with those of the native driver.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::Value as Json;
use snakemake_runtime::{JobResult, PlannedJob, Prepared, RunSettings, Settings, SnakemakeDriver};
use workflow_graph::{
    Delivery, Driver, DriverError, Files, Language, Launch, Note, Order, Outcome, OutputFile,
    RunOptions, Simulator, Task, TaskId, execute_with,
};

const SNAKEFILE: &str = r#"
SAMPLES = ["a", "b"]

rule all:
    input: expand("counts/{s}.txt", s=SAMPLES), "summary.txt"

rule make:
    output: temp("raw/{s}.txt")
    params: word=lambda wildcards: wildcards.s.upper()
    shell: "echo {params.word} > {output}"

rule count:
    input: "raw/{s}.txt"
    output: "counts/{s}.txt"
    log: "logs/{s}.log"
    retries: 1
    shell: "wc -c {input} > {output} 2> {log}"

rule summary:
    input: expand("counts/{s}.txt", s=SAMPLES)
    output: protected("summary.txt")
    run:
        with open(output[0], "w") as handle:
            handle.write(str(len(input)))
"#;

/// A driver that runs each planned job alone once its producers ended, as a wir run of
/// dialect tasks does, with the files of the producers as the inputs of the job.
struct Single {
    driver: SnakemakeDriver,
    jobs: Vec<PlannedJob>,
    files: Vec<Option<Vec<Json>>>,
    started: Vec<bool>,
    owners: HashMap<TaskId, usize>,
}

impl Single {
    fn new(driver: SnakemakeDriver) -> Single {
        Single {
            driver,
            jobs: Vec::new(),
            files: Vec::new(),
            started: Vec::new(),
            owners: HashMap::new(),
        }
    }

    fn ready(&mut self) -> Result<Vec<Task>, DriverError> {
        let mut tasks = Vec::new();
        let mut moved = true;
        while moved {
            moved = false;
            for index in 0..self.jobs.len() {
                let job = &self.jobs[index];
                let waits = (job.producers.iter().map(|producer| producer.job))
                    .chain(job.after.iter().copied())
                    .any(|other| self.files[other].is_none());
                if self.started[index] || waits {
                    continue;
                }
                let staged = (job.producers.iter())
                    .map(|producer| {
                        let made = self.files[producer.job].as_ref().expect("ended");
                        (
                            format!("input/{}", producer.input),
                            made[producer.output].clone(),
                        )
                    })
                    .collect();
                self.started[index] = true;
                moved = true;
                match self.driver.prepare_job(&job.key.clone(), staged)? {
                    Prepared::Run(task) => {
                        self.owners.insert(task.id, index);
                        tasks.push(*task);
                    }
                    Prepared::Skipped(made) => self.files[index] = Some(made),
                    Prepared::Failed(message) => return Err(DriverError::new(message)),
                }
            }
        }
        Ok(tasks)
    }
}

impl Driver for Single {
    fn language(&self) -> Language {
        Language::Snakemake
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        self.jobs = self.driver.start_jobs()?;
        self.files = vec![None; self.jobs.len()];
        self.started = vec![false; self.jobs.len()];
        self.ready()
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        let index = self.owners[&outcome.id];
        match self.driver.take_job(outcome)? {
            JobResult::Accepted(made) => {
                self.files[index] = Some(made);
                self.ready()
            }
            JobResult::Retry(task) => Ok(vec![*task]),
            JobResult::Failed(_) => Ok(Vec::new()),
        }
    }

    fn take_acceptance(&mut self, task: TaskId, attempt: u32) -> bool {
        self.driver.take_acceptance(task, attempt)
    }

    fn notes(&mut self) -> Vec<Note> {
        self.driver.notes()
    }

    fn deliveries(&mut self) -> Vec<Delivery> {
        self.driver.deliveries()
    }

    fn delivered(&mut self, delivery: &Delivery) -> Result<(), DriverError> {
        self.driver.delivered(delivery)
    }

    fn use_files(&mut self, files: Arc<dyn Files>) {
        self.driver.use_files(files);
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        self.driver.finish()
    }
}

fn launch(folder: &Path, simulated: bool) -> Launch {
    Launch {
        target: folder.join("Snakefile"),
        scratch: folder.join("scratch"),
        simulated,
        ..Launch::default()
    }
}

fn spawn(folder: &Path, simulated: bool) -> SnakemakeDriver {
    let run = RunSettings {
        workdir: Some(folder.to_owned()),
        ..RunSettings::default()
    };
    snakemake_runtime::spawn_driver(&launch(folder, simulated), Settings::default(), run)
        .expect("a driver")
}

/// The tasks of a simulated run as text with their acceptance, cache key and resume
/// fingerprint, with the folder of the run left out, sorted by fingerprint.
fn described(
    driver: &mut dyn Driver,
    folder: &Path,
) -> Vec<(String, Option<bool>, String, String)> {
    let mut executor = Simulator::new(folder.join("simulator"));
    let options = RunOptions {
        max_pending: 4,
        order: Order::AsAsked,
    };
    let execution = execute_with("adapter", driver, &mut executor, &options);
    let graph = execution.into_result().expect("the run succeeds");
    let shown = folder.to_string_lossy().into_owned();
    let mut tasks: Vec<_> = (graph.nodes.into_iter())
        .map(|node| {
            let task = Task {
                id: TaskId(0),
                after: Vec::new(),
                ..node.task
            };
            let text = serde_json::to_string(&task).expect("serializes");
            let text = text.replace(&shown, "<work>");
            let task: Task = serde_json::from_str(&text).expect("parses");
            let request = task_runner::request(&task, task.image.clone(), Vec::new(), None);
            // A rule without a directive, like `all`, runs no program and has no key.
            let key = request.spec.cache_key().map(|key| key.to_string());
            let key = key.unwrap_or_else(|error| format!("{error:?}"));
            let fingerprint = run_database::fingerprint(&task, &[]);
            (text, node.accepted, key, fingerprint)
        })
        .collect();
    tasks.sort_by(|one, other| one.3.cmp(&other.3));
    tasks
}

#[test]
fn single_jobs_match() {
    let native_folder = tempfile::tempdir().expect("a temporary directory");
    let single_folder = tempfile::tempdir().expect("a temporary directory");
    let mut runs = Vec::new();
    for (folder, single) in [(native_folder.path(), false), (single_folder.path(), true)] {
        std::fs::write(folder.join("Snakefile"), SNAKEFILE).expect("writes the Snakefile");
        let driver = spawn(folder, true);
        runs.push(match single {
            true => described(&mut Single::new(driver), folder),
            false => described(&mut { driver }, folder),
        });
    }
    assert_eq!(runs[0].len(), 6, "make and count twice, summary and all");
    for (native, single) in runs[0].iter().zip(&runs[1]) {
        assert_eq!(native, single);
    }
}

/// Feeds the same outcomes to the native driver and to the adapter: a failed attempt, then
/// one that misses its output, then one that makes it.
#[test]
fn retries_match() {
    let native_folder = tempfile::tempdir().expect("a temporary directory");
    let single_folder = tempfile::tempdir().expect("a temporary directory");
    let snakefile = "rule all:\n    input: \"out.txt\"\n\nrule make:\n    output: \"out.txt\"\n    retries: 2\n    shell: \"echo made > {output}\"\n";
    let mut runs = Vec::new();
    for folder in [native_folder.path(), single_folder.path()] {
        std::fs::write(folder.join("Snakefile"), snakefile).expect("writes the Snakefile");
        let made = folder.join("made.txt");
        std::fs::write(&made, "made\n").expect("writes the output");
        let outcomes = [
            (1, Vec::new()),
            (0, Vec::new()),
            (
                0,
                vec![OutputFile {
                    path: "out.txt".into(),
                    location: made.to_string_lossy().into_owned(),
                    ..OutputFile::default()
                }],
            ),
        ];
        let mut driver = spawn(folder, false);
        let mut seen = Vec::new();
        let single = folder == single_folder.path();
        let (mut task, key) = if single {
            let jobs = driver.start_jobs().expect("plans");
            let key = jobs
                .iter()
                .find(|job| job.key.rule == "make")
                .expect("make")
                .key
                .clone();
            match driver.prepare_job(&key, Vec::new()).expect("prepares") {
                Prepared::Run(task) => (*task, Some(key)),
                other => panic!("the job does not run: {other:?}"),
            }
        } else {
            (driver.start().expect("starts").remove(0), None)
        };
        for (attempt, (code, files)) in outcomes.into_iter().enumerate() {
            let attempt = attempt as u32 + 1;
            let outcome = Outcome {
                id: task.id,
                attempt,
                exit_code: code,
                files,
                ..Outcome::default()
            };
            let shown = Task {
                id: TaskId(0),
                ..task.clone()
            };
            seen.push(serde_json::to_string(&shown).expect("serializes"));
            let next = match key {
                Some(_) => match driver.take_job(outcome).expect("takes") {
                    JobResult::Retry(task) => Some(*task),
                    JobResult::Accepted(made) => {
                        seen.push(format!("accepted {}", made.len()));
                        None
                    }
                    JobResult::Failed(message) => panic!("fails: {message}"),
                },
                None => {
                    let mut tasks = driver.complete(outcome).expect("completes");
                    match tasks.iter().position(|next| next.step == "make") {
                        Some(position) => Some(tasks.remove(position)),
                        None => {
                            seen.push("accepted 1".to_owned());
                            None
                        }
                    }
                }
            };
            seen.push(format!("{}", driver.take_acceptance(task.id, attempt)));
            match next {
                Some(next) => task = next,
                None => break,
            }
        }
        let text = seen
            .join("\n")
            .replace(&folder.to_string_lossy()[..], "<work>");
        runs.push(text);
    }
    assert_eq!(runs[0], runs[1]);
    assert!(runs[0].ends_with("accepted 1\ntrue"), "{}", runs[0]);
}

#[test]
fn keys_name_jobs() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(folder.path().join("Snakefile"), SNAKEFILE).expect("writes the Snakefile");
    let jobs = spawn(folder.path(), true).plan_jobs().expect("plans");
    let count = (jobs.iter())
        .position(|job| job.key.rule == "count" && job.key.wildcards["s"] == "a")
        .expect("the count of a");
    assert_eq!(jobs[count].key.outputs, ["counts/a.txt", "logs/a.log"]);
    assert_eq!(jobs[count].inputs, ["raw/a.txt"]);
    let producer = jobs[count].producers[0];
    assert_eq!(jobs[producer.job].key.rule, "make");
    assert_eq!(jobs[producer.job].outputs[producer.output].flags, ["temp"]);
    assert!(jobs.iter().all(|job| job.needed && job.native.is_none()));
    let targets: Vec<&str> = (jobs.iter().filter(|job| job.target))
        .map(|job| job.key.rule.as_str())
        .collect();
    assert_eq!(targets, ["all"]);
}
