//! Checks the wr scheduler against a local fake of the REST API of a wr manager over HTTPS with
//! its own CA: the token and CA files, the job body, job states, and the removal of a job.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use executor::{Job, Staging};
use serde_json::json;
use slurm_executor::grid::{Scheduler, State, Submission};
use task_model::{FORMAT, Placement, TaskSpec};
use wr_executor::{Settings, Wr};

use common::FakeWr;

fn spec() -> TaskSpec {
    TaskSpec {
        output_manifest: None,
        run_as: None,
        format: FORMAT,
        namespace: String::new(),
        image: None,
        arguments: Vec::new(),
        workdir: None,
        environment: BTreeMap::new(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        stdin: None,
        containers: Vec::new(),
        replaces_entrypoint: false,
    }
}

/// Writes `text` as the job script of an attempt in `root`, as the grid runner does.
fn script(root: &Path, text: &str) -> std::path::PathBuf {
    let control = root.join("control");
    std::fs::create_dir_all(&control).expect("makes the folder");
    let path = control.join("job.sh");
    std::fs::write(&path, format!("#!/bin/bash\n{text}\n")).expect("writes the script");
    path
}

/// Waits until wr tells that the job `key` ended.
async fn ended(wr: &Wr, key: &str) -> State {
    loop {
        let states = wr.states(&[key.to_owned()]).await.expect("states");
        match states.get(key) {
            Some(State::Active) => tokio::time::sleep(Duration::from_millis(50)).await,
            Some(state) => return state.clone(),
            None => panic!("the job {key} is gone"),
        }
    }
}

/// uwe reads the token and the CA of the deployment, adds a job with the resources of its task,
/// which override what wr learned, and reads its end; a client with another CA is refused.
#[tokio::test]
async fn adds_tls_jobs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeWr::start(None);
    let home = folder.path().join("home");
    fake.write_files(&home, "production");
    let settings = Settings {
        endpoint: Some(fake.address.clone()),
        ..Settings::default()
    };
    let wr = Wr::connect(&settings, &home, 1000).expect("a client");
    let root = folder.path().join("run-ALIGN-7-1");
    let path = script(&root, "echo aligned > out.txt");
    let (spec, staging) = (spec(), Staging::default());
    let placement = Placement {
        cpus: 2,
        memory_bytes: Some(3 << 30),
        time_seconds: Some(3600),
        ..Placement::default()
    };
    let job = Job {
        name: "1a2b-ALIGN-7-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let submission = Submission {
        job,
        root: &root,
        script: &path,
        remote: false,
        margin: 300,
        proxy: None,
    };
    let key = wr.submit(&submission).await.expect("a job");
    assert_eq!(ended(&wr, &key).await, State::Ended("complete".into()));
    let output = std::fs::read_to_string(root.join("out.txt")).expect("the output");
    assert_eq!(output, "aligned\n");
    let added = fake.requests("POST /rest/v1/jobs/");
    assert_eq!(added.len(), 1);
    assert!(added[0].authorized);
    let body = &added[0].body[0];
    for (field, value) in [
        ("cwd", json!(root.to_string_lossy())),
        ("cwd_matters", json!(true)),
        ("rep_grp", json!("[uwe] 1a2b-ALIGN")),
        ("override", json!(2)),
        ("retries", json!(0)),
        ("cpus", json!(2)),
        ("memory", json!("3072M")),
        ("time", json!("3900s")),
    ] {
        assert_eq!(body[field], value, "{field}");
    }
    assert!(
        body["cmd"]
            .as_str()
            .is_some_and(|cmd| cmd.contains("job.sh"))
    );
    let (_, other) = common::authority();
    let stranger = Wr::new(&fake.address, common::TOKEN, other.as_bytes()).expect("a client");
    let refused = stranger.states(std::slice::from_ref(&key)).await;
    assert!(refused.is_err(), "{refused:?}");
    let unknown = Wr::new(&fake.address, "wrong", fake.ca.as_bytes()).expect("a client");
    let refused = unknown.submit(&submission).await;
    assert!(refused.is_err_and(|error| error.contains("401")));
}

/// A running job is stopped and then deleted from the queue, so that wr keeps no job of uwe.
#[tokio::test]
async fn removes_running_jobs() {
    let folder = tempfile::tempdir().expect("a folder");
    let fake = FakeWr::start(None);
    let wr = Wr::new(&fake.address, common::TOKEN, fake.ca.as_bytes()).expect("a client");
    let root = folder.path().join("run-LONG-1-1");
    let path = script(&root, "sleep 600");
    let (spec, staging, placement) = (spec(), Staging::default(), Placement::default());
    let job = Job {
        name: "1a2b-LONG-1-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let submission = Submission {
        job,
        root: &root,
        script: &path,
        remote: false,
        margin: 0,
        proxy: None,
    };
    let key = wr.submit(&submission).await.expect("a job");
    while fake.state(&key).as_deref() != Some("running") {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    wr.remove(&key).await.expect("removes the job");
    assert_eq!(fake.state(&key), None);
    let deletions: Vec<String> = (fake.requests("DELETE").into_iter())
        .map(|request| request.path)
        .collect();
    let path = format!("/rest/v1/jobs/{key}");
    let expected = [
        format!("{path}?state=running"),
        format!("{path}?state=deletable"),
    ];
    assert_eq!(deletions, expected);
    let body = &fake.requests("POST")[0].body[0];
    assert_eq!(body["override"], 0);
    assert!(body.get("memory").is_none());
}
