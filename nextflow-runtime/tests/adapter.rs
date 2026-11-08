//! The single-task adapter builds, accepts and retries tasks as the native driver does.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use nextflow_runtime::driver::{Nextflow, Settings};
use nextflow_runtime::{Adapter, Call, Completed, Prepared};
use serde_json::{Value as Json, json};
use workflow_graph::{Delivery, Driver, Launch, Outcome, OutputFile, Task, TaskId};

const PIPELINE: &str = r#"
process GREET {
    tag "${meta.id}"
    container 'debian:stable-slim'
    memory { 1.GB * task.attempt }
    errorStrategy { task.attempt < 2 ? 'retry' : 'ignore' }
    publishDir "${params.outdir}/greet", mode: 'copy'
    input:
    tuple val(meta), val(name)
    output:
    tuple val(meta), path("${name}.txt"), emit: text
    path 'versions.yml', optional: true
    script:
    def args = task.ext.args ?: ''
    """
    echo ${params.greeting} ${args} ${name} > ${name}.txt
    """
}

process SHOUT {
    input:
    tuple val(meta), path(text)
    val suffix
    output:
    path 'shout.txt'
    script:
    """
    tr a-z A-Z < ${text} > shout.txt
    echo ${suffix} >> shout.txt
    """
}

workflow {
    words = channel.of([[id: 'a'], 'hello'], [[id: 'b'], 'world'])
    SHOUT(GREET(words).text, '!')
}
"#;

const CONFIG: &str = r#"
params.greeting = 'hi'
params.outdir = 'results'
env.MODE = 'fast'
process {
    withName: 'GREET' {
        ext.args = { "--id ${meta.id}" }
    }
}
"#;

/// A host in the folder of the test that prints nothing.
struct Quiet(String);

impl nextflow_evaluator::interpreter::Host for Quiet {
    fn print(&mut self, _: &str) {}

    fn launch_dir(&self) -> String {
        self.0.clone()
    }
}

fn setup(directory: &Path) -> (Launch, Call, Call) {
    std::fs::write(directory.join("main.nf"), PIPELINE).unwrap();
    std::fs::write(directory.join("nextflow.config"), CONFIG).unwrap();
    let launch = Launch {
        target: directory.to_owned(),
        scratch: directory.join("scratch"),
        ..Launch::default()
    };
    let script = directory.join("main.nf").to_string_lossy().into_owned();
    let call = |name: &str| Call {
        script: script.clone(),
        process: name.into(),
        name: name.into(),
        singleton: false,
    };
    (launch, call("GREET"), call("SHOUT"))
}

fn settings(directory: &Path) -> Settings {
    Settings {
        host: Box::new(Quiet(directory.to_string_lossy().into_owned())),
        ..Settings::default()
    }
}

/// A finished attempt that made the given files in its work folder.
fn outcome(task: &Task, attempt: u32, exit_code: i32, files: &[&str]) -> Outcome {
    let made = files.iter().map(|name| OutputFile {
        path: (*name).into(),
        location: format!("/work/{}/{attempt}/{name}", task.id.0),
        ..OutputFile::default()
    });
    Outcome {
        id: task.id,
        attempt,
        exit_code,
        files: made.collect(),
        ..Outcome::default()
    }
}

fn run(prepared: Prepared) -> Task {
    match prepared {
        Prepared::Run(task) => *task,
        other => panic!("a task to run, not {other:?}"),
    }
}

fn accepted(completed: Completed) -> (Vec<Option<Json>>, Vec<Delivery>) {
    match completed {
        Completed::Accepted {
            outputs,
            deliveries,
        } => (outputs, deliveries),
        other => panic!("an accepted attempt, not {other:?}"),
    }
}

/// The task as the resume database and the task cache see it, which must match.
fn same(native: &Task, adapted: &Task) {
    assert_eq!(native, adapted, "the adapter builds the native task");
    let native = run_database::fingerprint(native, &[]);
    assert_eq!(native, run_database::fingerprint(adapted, &[]));
}

#[test]
fn matches_native_tasks() {
    let directory = tempfile::tempdir().unwrap();
    let (launch, greet, shout) = setup(directory.path());
    let mut native = Nextflow::new(&launch, settings(directory.path())).unwrap();
    let mut adapter = Adapter::new(&launch, settings(directory.path()), json!({})).unwrap();
    let started = native.start().unwrap();
    assert_eq!(started.len(), 2, "one task for each word");
    let script = started[0].script.as_deref().unwrap_or_default();
    assert!(
        script.contains("hi --id a hello"),
        "the selector sets ext.args: {script}"
    );
    let words = [json!([{"id": "a"}, "hello"]), json!([{"id": "b"}, "world"])];
    for (index, (task, word)) in started.iter().zip(&words).enumerate() {
        let index = index as u32 + 1;
        let prepared = adapter.prepare(&greet, std::slice::from_ref(word), index, task.id);
        same(task, &run(prepared.unwrap()));
    }
    let first = &started[0];
    let shouted = native
        .complete(outcome(first, 1, 0, &["hello.txt"]))
        .unwrap();
    let published = native.deliveries();
    let completed = adapter.complete(outcome(first, 1, 0, &["hello.txt"]));
    let (outputs, deliveries) = accepted(completed.unwrap());
    assert_eq!(deliveries, published, "publishDir publishes the same files");
    assert_eq!(outputs[1], None, "a missing optional output is empty");
    let text = outputs[0].clone().expect("the text output");
    assert_eq!(text[1]["location"], "/work/1/1/hello.txt");
    let inputs = [text, json!("!")];
    let prepared = adapter.prepare(&shout, &inputs, 1, shouted[0].id);
    same(&shouted[0], &run(prepared.unwrap()));
}

#[test]
fn matches_native_retries() {
    let directory = tempfile::tempdir().unwrap();
    let (launch, greet, _) = setup(directory.path());
    let mut native = Nextflow::new(&launch, settings(directory.path())).unwrap();
    let mut adapter = Adapter::new(&launch, settings(directory.path()), json!({})).unwrap();
    let started = native.start().unwrap();
    let word = [json!([{"id": "b"}, "world"])];
    run(adapter.prepare(&greet, &word, 2, started[1].id).unwrap());
    // A missing required output fails the attempt like a failed command.
    let retried = native.complete(outcome(&started[1], 1, 0, &[])).unwrap();
    let completed = adapter.complete(outcome(&started[1], 1, 0, &[])).unwrap();
    let Completed::Retry(task) = completed else {
        panic!("a retry, not {completed:?}")
    };
    assert_eq!(retried.len(), 1, "the native driver retries too");
    same(&retried[0], &task);
    assert_eq!(task.resources.memory_bytes, Some(2 << 30), "attempt 2");
    let ignored = native.complete(outcome(&task, 2, 3, &[])).unwrap();
    assert!(ignored.is_empty(), "the second failure is ignored");
    let completed = adapter.complete(outcome(&task, 2, 3, &[])).unwrap();
    let Completed::Ignored(message) = completed else {
        panic!("an ignored failure, not {completed:?}")
    };
    assert!(message.contains("exit status (3)"), "{message}");
    assert!(!native.take_acceptance(task.id, 2));
}

#[test]
fn refuses_unknown_processes() {
    let directory = tempfile::tempdir().unwrap();
    let (launch, mut greet, _) = setup(directory.path());
    let mut adapter = Adapter::new(&launch, settings(directory.path()), json!({})).unwrap();
    greet.process = "MISSING".into();
    let error = adapter.prepare(&greet, &[], 1, TaskId(1)).unwrap_err();
    assert!(
        error.message.contains("no process MISSING"),
        "{}",
        error.message
    );
}

#[test]
fn finds_normal_scripts() {
    let directory = tempfile::tempdir().unwrap();
    let (launch, mut greet, _) = setup(directory.path());
    let mut adapter = Adapter::new(&launch, settings(directory.path()), json!({})).unwrap();
    let folder = directory.path().join("modules");
    greet.script = folder.join("../main.nf").to_string_lossy().into_owned();
    let word = [json!([{"id": "a"}, "hello"])];
    run(adapter.prepare(&greet, &word, 1, TaskId(1)).unwrap());
}
