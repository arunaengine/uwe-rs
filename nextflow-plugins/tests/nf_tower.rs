//! nf-tower against the requests that nf-tower 1.28.4 sent with Nextflow 26.04.6 to a local
//! fake server (`req-*.log`, one JSON line per request); `main.nf` publishes below its folder.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;
#[path = "common/fake.rs"]
mod fake;

use std::path::{Path, PathBuf};

use base64::Engine as _;
use fake::{Fake, Request, keys, recorded};
use md5::{Digest, Md5};
use serde_json::{Value, json};

/// A run of `script` with nf-tower reporting to `fake`, and the requests it sent.
fn run(script: &str, fake: &Fake, extra: &str) -> (tempfile::TempDir, Option<String>) {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    common::copy(
        &common::fixtures("nf_tower"),
        folder,
        &[script, "tower.yml"],
    );
    if script != "main.nf" {
        std::fs::rename(folder.join(script), folder.join("main.nf")).expect("renames");
    }
    let config = format!(
        "plugins {{ id 'nf-tower@1.28.4' }}\nmanifest {{ name = 'demo/probe'; version = '1.0.0'; \
         description = 'probe' }}\ntower {{\n  enabled = true\n  endpoint = '{}/'\n  \
         accessToken = 'not-a-real-token'\n  workspaceId = '12345'\n{extra}}}\n",
        fake.address
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, _) = common::run(folder, "lethal_legentil");
    (directory, execution.error.map(|error| error.to_string()))
}

/// Routes that create the run as `id`, like `routes.json` of the fixtures.
fn routes(id: &str) -> Value {
    json!([
        {"method": "POST", "match": "/trace/create",
         "body": {"workflowId": id, "watchUrl": format!("http://127.0.0.1/watch/{id}")}},
        {"match": "/", "body": {}}
    ])
}

/// A workflow id of this test run, so that parallel runs write their own reports file.
fn unique(name: &str) -> String {
    format!("{name}{}", std::process::id())
}

/// Removes the reports file that a run writes into the launch directory, the crate folder.
struct Removed(PathBuf);

impl Drop for Removed {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn path(request: &Request) -> &str {
    request.path.split('?').next().unwrap_or_default()
}

/// The latest record of each task over all progress requests.
fn tasks(requests: &[Request]) -> Vec<Value> {
    let mut latest: Vec<Value> = Vec::new();
    for request in requests
        .iter()
        .filter(|request| path(request).ends_with("/progress"))
    {
        for task in request.json()["tasks"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            latest.retain(|kept| kept["taskId"] != task["taskId"]);
            latest.push(task);
        }
    }
    latest.sort_by_key(|task| task["taskId"].as_i64());
    latest
}

#[test]
fn reports_like_platform() {
    let id = unique("ok");
    let reports = Removed(
        std::env::current_dir()
            .expect("a folder")
            .join(format!("nf-{id}-reports.tsv")),
    );
    let fake = Fake::start(routes(&id));
    let (directory, error) = run("main.nf", &fake, "");
    assert_eq!(error, None);
    let requests = fake.requests();
    let order: Vec<String> = (requests.iter())
        .map(|request| format!("{} {}", request.method, path(request)))
        .filter(|line| !line.ends_with("/progress"))
        .collect();
    assert_eq!(
        order,
        [
            "POST /trace/create".to_owned(),
            format!("PUT /trace/{id}/begin"),
            format!("PUT /trace/{id}/complete"),
        ]
    );
    let expected = recorded(include_str!("fixtures/nf_tower/req-success.log"));
    let token = base64::engine::general_purpose::STANDARD.encode("@token:not-a-real-token");
    for request in &requests {
        assert!(
            request.path.ends_with("?workspaceId=12345"),
            "{}",
            request.path
        );
        assert_eq!(
            request.header("Authorization"),
            Some(format!("Basic {token}").as_str())
        );
        assert_eq!(request.header("User-Agent"), Some("Nextflow/26.04.6"));
        assert_eq!(
            request.header("Content-Type"),
            Some("application/json; charset=utf-8")
        );
        let trace = request.header("Traceparent").unwrap_or_default();
        assert!(trace.starts_with("00-") && trace.len() == 55, "{trace}");
    }

    let create = requests[0].json();
    assert_eq!(keys(&create), keys(&expected[0]["body"]));
    assert_eq!(create["runName"], "lethal_legentil");
    assert_eq!(create["workflowId"], Value::Null);

    let begin = requests[1].json();
    let recorded_begin = &expected[1]["body"];
    let mut begin_keys = keys(&begin);
    begin_keys.retain(|key| !key.starts_with(".dag"));
    assert_eq!(begin_keys, keys(recorded_begin));
    let workflow = &begin["workflow"];
    assert_eq!(begin["processNames"], json!(["SAY", "JOIN"]));
    assert_eq!(begin["towerLaunch"], false);
    assert_eq!(workflow["manifest"], recorded_begin["workflow"]["manifest"]);
    assert_eq!(workflow["nextflow"], recorded_begin["workflow"]["nextflow"]);
    assert_eq!(workflow["params"], json!({"greeting": "hello"}));
    assert_eq!(workflow["id"], id.as_str());
    assert_eq!(workflow["platform"]["workflowId"], id.as_str());
    let script = std::fs::read(directory.path().join("main.nf")).expect("the script");
    let digest: String = Md5::digest(script)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(workflow["scriptId"], digest.as_str());
    let config = workflow["configText"].as_str().unwrap_or_default();
    assert!(config.contains("   accessToken = '[secret]'\n"), "{config}");
    assert!(!config.contains("not-a-real-token"), "{config}");
    assert_eq!(begin["dag"]["vertices"][1]["label"], "JOIN");
    assert_eq!(
        begin["dag"]["edges"],
        json!([{"source": "1", "target": "2", "label": null}])
    );

    let recorded_tasks = expected[2]["body"]["tasks"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let tasks = tasks(&requests);
    let named: Vec<(&str, &str)> = (tasks.iter())
        .map(|task| {
            let status = task["status"].as_str().unwrap_or_default();
            (task["name"].as_str().unwrap_or_default(), status)
        })
        .collect();
    let mut sorted = named.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        [
            ("JOIN", "COMPLETED"),
            ("SAY (a)", "COMPLETED"),
            ("SAY (b)", "COMPLETED")
        ]
    );
    for task in &tasks {
        assert_eq!(keys(task), keys(&recorded_tasks[0]));
        assert_eq!(task["executor"], "local");
        assert!(
            task["submit"]
                .as_str()
                .is_some_and(|time| time.starts_with("20"))
        );
    }

    let complete = requests.last().expect("a request").json();
    let recorded_complete = &expected[3]["body"];
    let mut complete_keys = keys(&complete);
    complete_keys.retain(|key| !key.starts_with(".metrics."));
    let mut recorded_keys = keys(recorded_complete);
    recorded_keys.retain(|key| !key.starts_with(".metrics."));
    assert_eq!(complete_keys, recorded_keys);
    let workflow = &complete["workflow"];
    assert_eq!(workflow["success"], true);
    assert_eq!(workflow["exitStatus"], 0);
    assert_eq!(workflow["stats"]["succeededCount"], 3);
    assert_eq!(workflow["stats"]["succeedPct"], 100.0);
    assert_eq!(complete["progress"]["succeeded"], 3);
    assert_eq!(complete["progress"]["processes"][0]["terminated"], true);
    let metrics = complete["metrics"].as_array().cloned().unwrap_or_default();
    let processes: Vec<&Value> = metrics.iter().map(|metric| &metric["process"]).collect();
    assert_eq!(processes, [&json!("SAY"), &json!("JOIN")]);
    let series: Vec<&String> = metrics[0]
        .as_object()
        .map(|map| map.keys().collect())
        .unwrap();
    let recorded_series: Vec<&String> = (recorded_complete["metrics"][0].as_object())
        .map(|map| map.keys().collect())
        .unwrap_or_default();
    assert_eq!(series, recorded_series);
    assert_eq!(
        metrics[0]["time"]["minLabel"]
            .as_str()
            .map(|name| &name[..3]),
        Some("SAY")
    );

    let text = std::fs::read_to_string(&reports.0).expect("the reports file");
    let folder = directory.path().display();
    let mut lines: Vec<&str> = text.lines().collect();
    lines[1..].sort();
    assert_eq!(
        lines,
        [
            "key\tpath\tsize\tdisplay\tmime_type".to_owned(),
            format!("*.txt\t{folder}/results/a.txt\t8\tGreeting\t"),
            format!("*.txt\t{folder}/results/b.txt\t8\tGreeting\t"),
            format!("report.html\t{folder}/results/report.html\t16\tJoined report\ttext/html"),
        ]
    );
}

#[test]
fn reports_failed_run() {
    let id = unique("bad");
    let _reports = Removed(Path::new(&format!("nf-{id}-reports.tsv")).to_owned());
    let fake = Fake::start(routes(&id));
    let (_directory, error) = run("fail.nf", &fake, "");
    assert!(error.is_some());
    let requests = fake.requests();
    let complete = requests.last().expect("a request");
    assert_eq!(path(complete), format!("/trace/{id}/complete"));
    let complete = complete.json();
    let workflow = &complete["workflow"];
    assert_eq!(workflow["success"], false);
    assert_eq!(workflow["exitStatus"], 3);
    assert_eq!(workflow["errorMessage"], "boom");
    assert_eq!(workflow["stats"]["failedCount"], 1);
    let metrics: Vec<&Value> = (complete["metrics"].as_array().into_iter().flatten())
        .map(|metric| &metric["process"])
        .collect();
    assert_eq!(metrics, [&json!("SLOW"), &json!("BAD")]);
    let statuses: Vec<(i64, String)> = (tasks(&requests).iter())
        .map(|task| {
            let status = task["status"].as_str().unwrap_or_default().to_owned();
            (task["taskId"].as_i64().unwrap_or_default(), status)
        })
        .collect();
    assert_eq!(
        statuses,
        [(1, "COMPLETED".to_owned()), (2, "FAILED".to_owned())]
    );
    let recorded = recorded(include_str!("fixtures/nf_tower/req-fail.log"));
    let last = recorded.last().expect("a recorded request");
    assert_eq!(keys(&complete["workflow"]), keys(&last["body"]["workflow"]));
}

#[test]
fn stops_unauthorized_run() {
    let fake = Fake::start(json!([
        {"method": "POST", "match": "/trace/create", "status": 401,
         "body": {"message": "Unauthorized"}}
    ]));
    let (_directory, error) = run("main.nf", &fake, "");
    let error = error.expect("the run fails");
    let expected = format!(
        "Abort exception produced when notifying an event - Unexpected HTTP response\n\
         - endpoint    : {}/trace/create\n- status code : 401\n\
         - response msg: Unauthorized Seqera Platform API access -- Make sure you have \
         specified the correct access token",
        fake.address
    );
    assert!(error.contains(&expected), "{error}");
    assert_eq!(fake.requests().len(), 1);
}

#[test]
fn aborts_failed_progress() {
    let id = unique("down");
    let _reports = Removed(Path::new(&format!("nf-{id}-reports.tsv")).to_owned());
    let fake = Fake::start(json!([
        {"method": "POST", "match": "/trace/create", "body": {"workflowId": id}},
        {"method": "PUT", "match": "/begin", "body": {"watchUrl": "http://127.0.0.1/watch"}},
        {"method": "PUT", "match": "/progress", "status": 503, "type": "text/html",
         "body": "<html><head><title>503 Service Unavailable</title></head><body>down</body></html>"},
    ]));
    let retry = "  retryPolicy { maxAttempts = 3; delay = '200ms' }\n";
    let (_directory, error) = run("main.nf", &fake, retry);
    let error = error.expect("the run fails");
    let expected = format!(
        "Aborting session due to Seqera Platform telemetry exception - Unexpected HTTP \
         response\n- endpoint    : {}/trace/{id}/progress\n\
         - status code : 503\n- response msg: 503 Service Unavailable",
        fake.address
    );
    assert!(error.contains(&expected), "{error}");
    let paths: Vec<String> = fake.requests().iter().map(|r| path(r).to_owned()).collect();
    let progress = format!("/trace/{id}/progress");
    assert_eq!(paths.iter().filter(|path| **path == progress).count(), 3);
    assert!(
        !paths.iter().any(|path| path.ends_with("/complete")),
        "{paths:?}"
    );
}
