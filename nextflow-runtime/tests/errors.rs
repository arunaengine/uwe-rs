//! What a Nextflow run does when a task fails, by its `errorStrategy`: retry it with its
//! attempt counted up, ignore it, finish the running tasks first, or stop at once.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;

use workflow_graph::{
    Attempt, Engine, ExecuteError, Executor, Launch, Order, Outcome, RunOptions, Running,
};

/// Fails the attempts it is told to with their exit codes, and notes the step, attempt and
/// memory of each attempt.
#[derive(Default)]
struct Scripted {
    exit_codes: HashMap<(String, u32), i32>,
    ran: Vec<(String, u32, Option<u64>)>,
}

impl Executor for Scripted {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        let step = task.step.rsplit(':').next().unwrap_or_default().to_owned();
        let memory = task.resources.memory_bytes.map(|bytes| bytes >> 30);
        self.ran.push((step.clone(), attempt.number, memory));
        let code = self.exit_codes.get(&(step, attempt.number)).copied();
        Running::ended(Ok(Outcome {
            id: task.id,
            exit_code: code.unwrap_or(0),
            ..Outcome::default()
        }))
    }
}

const PIPELINE: &str = r#"
process FIRST {
    input:
    val x
    output:
    val x
    script:
    "echo ${x}"
}

process SECOND {
    input:
    val x
    script:
    "echo ${x}"
}

workflow {
    SECOND(FIRST(channel.of(1)))
}
"#;

/// Runs the pipeline with this config and these failures; the result has the error and what
/// ran.
fn run(
    config: &str,
    failures: &[(&str, u32, i32)],
) -> (Option<String>, Vec<(String, u32, Option<u64>)>) {
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(directory.path().join("main.nf"), PIPELINE).expect("writes the script");
    std::fs::write(directory.path().join("nextflow.config"), config).expect("writes the config");
    let launch = Launch {
        target: directory.path().to_owned(),
        ..Launch::default()
    };
    let mut driver = nextflow_runtime::driver(&launch).expect("a driver");
    let mut executor = Scripted {
        exit_codes: (failures.iter())
            .map(|(step, attempt, code)| (((*step).to_owned(), *attempt), *code))
            .collect(),
        ..Scripted::default()
    };
    let options = RunOptions {
        max_pending: 4,
        order: Order::AsAsked,
    };
    let execution = Engine::new(&mut executor)
        .options(options)
        .run("main.nf", driver.as_mut());
    let error = execution.error.map(|error: ExecuteError| error.to_string());
    (error, executor.ran)
}

const RETRY: &str = r#"
process {
    errorStrategy = { task.exitStatus in 137..140 ? 'retry' : 'terminate' }
    maxRetries = 2
    memory = { 1.GB * task.attempt }
}
"#;

#[test]
fn failed_tasks_run_again_with_more_memory() {
    let (error, ran) = run(RETRY, &[("FIRST", 1, 137), ("FIRST", 2, 137)]);
    assert_eq!(error, None);
    let expected = [
        ("FIRST".to_owned(), 1, Some(1)),
        ("FIRST".to_owned(), 2, Some(2)),
        ("FIRST".to_owned(), 3, Some(3)),
        ("SECOND".to_owned(), 1, Some(1)),
    ];
    assert_eq!(ran, expected);
    // Out of retries, or with an exit code the closure does not retry, the run stops.
    let (error, _) = run(
        RETRY,
        &[("FIRST", 1, 137), ("FIRST", 2, 137), ("FIRST", 3, 137)],
    );
    let error = error.expect("an error");
    assert!(
        error.contains("terminated with an error exit status (137)"),
        "{error}"
    );
    let (error, ran) = run(RETRY, &[("FIRST", 1, 1)]);
    assert!(error.is_some());
    assert_eq!(ran.len(), 1);
}

#[test]
fn ignored_failures_leave_the_outputs_empty() {
    let config = "process.errorStrategy = 'ignore'\n";
    let (error, ran) = run(config, &[("FIRST", 1, 2)]);
    assert_eq!(error, None);
    assert_eq!(ran, [("FIRST".to_owned(), 1, None)]);
}

#[test]
fn finish_fails_the_run_once_the_running_tasks_end() {
    let config = "process.errorStrategy = 'finish'\n";
    let (error, ran) = run(config, &[("FIRST", 1, 3)]);
    let error = error.expect("an error");
    assert!(
        error.contains("terminated with an error exit status (3)"),
        "{error}"
    );
    assert_eq!(ran, [("FIRST".to_owned(), 1, None)]);
}
