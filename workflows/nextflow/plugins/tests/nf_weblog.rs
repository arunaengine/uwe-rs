//! nf-weblog against the messages that nf-weblog 1.2.0 posted with Nextflow 26.04.6 to a local
//! fake server (`req-*.log`), for the probe pipelines of the nf-tower fixtures.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;
#[path = "common/fake.rs"]
mod fake;

use base64::Engine as _;
use fake::{Fake, Request, keys, recorded};
use serde_json::{Value, json};

/// A run of `script` with nf-weblog posting to `fake`, and the error that failed it.
fn run(script: &str, fake: &Fake, token: &str) -> Option<String> {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    common::copy(&common::fixtures("nf_tower"), folder, &[script]);
    if script != "main.nf" {
        std::fs::rename(folder.join(script), folder.join("main.nf")).expect("renames");
    }
    let config = format!(
        "plugins {{ id 'nf-weblog@1.2.0' }}\nmanifest {{ name = 'demo/probe'; version = \
         '1.0.0' }}\nweblog {{\n  enabled = true\n  url = '{}/hook'\n  basicToken = \
         '{token}'\n}}\n",
        fake.address
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, _) = common::run(folder, "backstabbing_ride");
    execution.error.map(|error| error.to_string())
}

fn event(request: &Request) -> String {
    request.json()["event"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// The recorded message of an event, the first one of a task status when one is given.
fn sample<'a>(messages: &'a [Value], name: &str, status: Option<&str>) -> &'a Value {
    (messages.iter())
        .map(|message| &message["body"])
        .find(|body| {
            body["event"] == name && status.is_none_or(|status| body["trace"]["status"] == status)
        })
        .expect("a recorded message")
}

fn trace_keys(message: &Value) -> Vec<String> {
    let mut found = keys(&message["trace"]);
    found.retain(|key| key != ".native_id");
    found
}

#[test]
fn posts_run_events() {
    let fake = Fake::start(json!([{"match": "/", "body": "ok", "type": "text/plain"}]));
    assert_eq!(run("main.nf", &fake, "user:not-a-real-password"), None);
    let requests = fake.requests();
    let token = base64::engine::general_purpose::STANDARD.encode("user:not-a-real-password");
    for request in &requests {
        assert_eq!(
            (request.method.as_str(), request.path.as_str()),
            ("POST", "/hook")
        );
        assert_eq!(
            request.header("Authorization"),
            Some(format!("Basic {token}").as_str())
        );
        assert_eq!(request.header("Content-Type"), Some("application/json"));
    }
    let events: Vec<String> = requests.iter().map(event).collect();
    assert_eq!(events.len(), 11, "{events:?}");
    assert_eq!(events.first().map(String::as_str), Some("started"));
    assert_eq!(events.last().map(String::as_str), Some("completed"));
    for name in ["process_submitted", "process_started", "process_completed"] {
        assert_eq!(events.iter().filter(|event| *event == name).count(), 3);
    }

    let expected = recorded(include_str!("fixtures/nf_weblog/req-success.log"));
    let started = requests[0].json();
    let recorded_started = sample(&expected, "started", None);
    let mut started_keys = keys(&started);
    started_keys.retain(|key| !key.starts_with(".metadata.parameters."));
    assert_eq!(started_keys, keys(recorded_started));
    // uwe knows the params of the script at the start, where Nextflow sends none yet.
    assert_eq!(
        started["metadata"]["parameters"],
        json!({"greeting": "hello"})
    );
    assert_eq!(started["runName"], "backstabbing_ride");
    let workflow = &started["metadata"]["workflow"];
    assert_eq!(workflow["container"], json!({}));
    assert_eq!(workflow["complete"], Value::Null);
    let time = started["utcTime"].as_str().unwrap_or_default();
    assert!(time.len() == 20 && time.ends_with('Z'), "{time}");

    for (name, status) in [
        ("process_submitted", "SUBMITTED"),
        ("process_started", "RUNNING"),
        ("process_completed", "COMPLETED"),
    ] {
        let sent = (requests.iter())
            .map(Request::json)
            .find(|body| body["event"] == name)
            .expect("a message");
        let recorded = sample(&expected, name, Some(status));
        assert_eq!(trace_keys(&sent), trace_keys(recorded), "{name}");
        assert_eq!(sent["trace"]["status"], status);
        let exit = if status == "COMPLETED" { 0 } else { 2147483647 };
        assert_eq!(sent["trace"]["exit"], exit);
    }

    let completed = requests.last().expect("a message").json();
    let recorded_completed = sample(&expected, "completed", None);
    assert_eq!(keys(&completed), keys(recorded_completed));
    let workflow = &completed["metadata"]["workflow"];
    assert_eq!(workflow["success"], true);
    assert_eq!(workflow["stats"]["succeededCount"], 3);
    assert_eq!(workflow["stats"]["processes"][1]["name"], "JOIN");
    assert_eq!(workflow["stats"]["processes"][1]["taskName"], "JOIN");
    assert!(
        workflow["complete"]["year"]
            .as_i64()
            .is_some_and(|year| year >= 2026)
    );
}

#[test]
fn posts_failed_run() {
    let fake = Fake::start(json!([{"match": "/", "body": "ok", "type": "text/plain"}]));
    assert!(run("fail.nf", &fake, "user:not-a-real-password").is_some());
    let requests = fake.requests();
    let events: Vec<String> = requests.iter().map(event).collect();
    assert_eq!(
        events,
        [
            "started",
            "process_submitted",
            "process_started",
            "process_completed",
            "process_submitted",
            "process_started",
            "process_completed",
            "error",
            "completed",
        ]
    );
    let error = requests[7].json();
    assert_eq!(error["trace"]["name"], "BAD");
    assert_eq!(error["trace"]["status"], "FAILED");
    assert_eq!(error["trace"]["exit"], 3);
    let completed = requests[8].json();
    let workflow = &completed["metadata"]["workflow"];
    assert_eq!(workflow["success"], false);
    assert_eq!(workflow["exitStatus"], 3);
    assert_eq!(workflow["errorMessage"], "boom");
    let expected = recorded(include_str!("fixtures/nf_weblog/req-fail.log"));
    assert_eq!(keys(&completed), keys(sample(&expected, "completed", None)));
}

#[test]
fn ignores_refused_messages() {
    let fake = Fake::start(json!([
        {"match": "/", "status": 500, "body": "fake failure", "type": "text/plain"}
    ]));
    assert_eq!(run("main.nf", &fake, "user:not-a-real-password"), None);
    assert_eq!(fake.requests().len(), 11, "one attempt per message");
}

#[test]
fn rejects_bad_token() {
    let fake = Fake::start(json!([]));
    let error = run("main.nf", &fake, "no-colon").expect("the run fails");
    assert!(
        error.contains("Invalid auth token provided - should be 'USERNAME:PASSWORD'."),
        "{error}"
    );
    assert!(fake.requests().is_empty());
}
