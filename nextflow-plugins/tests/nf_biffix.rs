//! nf-biffix against the events that nf-biffix 0.6.2 wrote and posted with Nextflow 26.04.6 for
//! the probe pipelines `main.nf` and `fail.nf` (`events-*.jsonl`), to a local fake ingest route.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;
#[path = "common/fake.rs"]
mod fake;

use std::path::Path;
use std::process::Command;

use fake::{Fake, recorded};
use serde_json::{Value, json};

/// The plugin reads only environment variables, so each run is a child process of this test
/// binary, which runs `child_run` with them and the run's folder and script.
fn run(script: &str, fake: &Fake, name: &str) -> (tempfile::TempDir, bool) {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    common::copy(&common::fixtures("nf_biffix"), folder, &[script]);
    if script != "main.nf" {
        std::fs::rename(folder.join(script), folder.join("main.nf")).expect("renames");
    }
    std::fs::write(
        folder.join("nextflow.config"),
        "plugins { id 'nf-biffix@0.6.2' }\n",
    )
    .expect("writes the config");
    let child = Command::new(std::env::current_exe().expect("this test binary"))
        .args(["--exact", "child_run", "--ignored", "--test-threads=1"])
        .current_dir(folder)
        .env("BIFFIX_RUN_ID", "run-demo-0001")
        .env("BIFFIX_INGEST_URL", format!("{}/ingest", fake.address))
        .env("BIFFIX_INGEST_TOKEN", "not-a-real-token")
        .env("BIFFIX_TEST_RUN", name)
        .output()
        .expect("runs the child");
    let printed = String::from_utf8_lossy(&child.stdout);
    assert!(child.status.success(), "{printed}");
    (directory, child.status.success())
}

/// The run of a parent test, in the folder that is its current directory.
#[test]
#[ignore = "a child of the other tests, which set its environment"]
fn child_run() {
    let Ok(name) = std::env::var("BIFFIX_TEST_RUN") else {
        return;
    };
    let folder = std::env::current_dir().expect("the run folder");
    common::run(&folder, &name);
}

fn events(folder: &Path) -> Vec<Value> {
    recorded(&std::fs::read_to_string(folder.join(".biffix_events.jsonl")).expect("events"))
}

fn kinds(events: &[Value]) -> Vec<String> {
    let mut kinds: Vec<String> = (events.iter())
        .map(|event| event["kind"].as_str().unwrap_or_default().to_owned())
        .collect();
    kinds.sort();
    kinds
}

fn posted(fake: &Fake) -> Vec<Value> {
    (fake.requests().iter())
        .flat_map(|request| {
            request.json()["events"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .collect()
}

#[test]
fn reports_run() {
    let fake = Fake::start(json!([{"match": "/events", "body": {"accepted": 1}}]));
    let (directory, finished) = run("main.nf", &fake, "biffix_success");
    assert!(finished);
    let made = events(directory.path());
    let expected = recorded(include_str!("fixtures/nf_biffix/events-success.jsonl"));
    assert_eq!(kinds(&made), kinds(&expected));
    for event in &made {
        assert_eq!(
            (event["schema_version"].clone(), event["run_id"].clone()),
            (json!(1), json!("run-demo-0001"))
        );
        assert!(event["ts"].as_f64().is_some_and(|ts| ts > 1.7e9));
    }
    let join = (made.iter())
        .find(|event| event["kind"] == "task_created" && event["task_key"] == "JOIN")
        .expect("JOIN is created");
    assert_eq!(join["deps"], json!(["SAY (a) #1", "SAY (b) #2"]));
    let dag = made
        .iter()
        .find(|event| event["kind"] == "run_dag")
        .expect("the DAG");
    assert_eq!(dag["dag"], expected[7]["dag"]);
    assert_eq!(
        made.last().map(|event| event["kind"].clone()),
        Some(json!("run_succeeded"))
    );
    for request in fake.requests() {
        assert_eq!(
            (request.method.as_str(), request.path.as_str()),
            ("POST", "/ingest/events")
        );
        assert_eq!(
            request.header("Authorization"),
            Some("Bearer not-a-real-token")
        );
    }
    assert_eq!(posted(&fake), made, "the file holds what was posted");
}

#[test]
fn reports_failure() {
    let fake = Fake::start(json!([{"match": "/events", "body": {"accepted": 1}}]));
    let (directory, finished) = run("fail.nf", &fake, "biffix_failure");
    assert!(finished);
    let made = events(directory.path());
    let expected = recorded(include_str!("fixtures/nf_biffix/events-fail.jsonl"));
    assert_eq!(kinds(&made), kinds(&expected));
    let failed = made
        .iter()
        .find(|event| event["kind"] == "task_failed")
        .expect("a failure");
    assert_eq!(
        (failed["task_key"].clone(), failed["attempt"].clone()),
        (json!("BAD"), json!(1))
    );
    assert_eq!(
        made.last().map(|event| event["kind"].clone()),
        Some(json!("run_failed"))
    );
}

#[test]
fn stops_after_refusal() {
    let fake =
        Fake::start(json!([{"match": "/events", "status": 401, "body": {"error": "bad token"}}]));
    let (directory, finished) = run("main.nf", &fake, "biffix_refused");
    assert!(finished);
    assert_eq!(fake.requests().len(), 1, "posting stops after the refusal");
    assert_eq!(
        events(directory.path()).len(),
        9,
        "the file keeps every event"
    );
}

#[test]
fn splits_rejected_batches() {
    let fake = Fake::start(json!([{"match": "/events", "status": 413, "body": "too large"}]));
    let (directory, finished) = run("main.nf", &fake, "biffix_rejected");
    assert!(finished);
    let sizes: Vec<usize> = (fake.requests().iter())
        .map(|request| request.json()["events"].as_array().map_or(0, Vec::len))
        .collect();
    let single = sizes.iter().filter(|size| **size == 1).count();
    assert_eq!(
        single, 9,
        "each event is tried alone before it is dropped: {sizes:?}"
    );
    assert_eq!(events(directory.path()).len(), 9);
}
