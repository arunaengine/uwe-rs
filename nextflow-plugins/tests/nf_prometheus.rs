//! nf-prometheus against the metrics files and pushes that nf-prometheus 0.1.0 wrote with
//! Nextflow 26.04.6, for the probe pipelines of the nf-tower fixtures.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;
#[path = "common/fake.rs"]
mod fake;

use fake::{Fake, recorded};
use serde_json::json;

const RUN: &str = "scruffy_meninsky";

/// A run of `script` that pushes to `fake`, with the metrics file it wrote and its error.
fn run(script: &str, fake: &Fake) -> (String, Option<String>) {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    common::copy(&common::fixtures("nf_tower"), folder, &[script]);
    if script != "main.nf" {
        std::fs::rename(folder.join(script), folder.join("main.nf")).expect("renames");
    }
    let metrics = folder.join("metrics/run.prom");
    let config = format!(
        "plugins {{ id 'nf-prometheus@0.1.0' }}\nprometheus {{\n  path = '{}'\n  pushgateway \
         = '{}/'\n  pushJob = 'nf job'\n}}\n",
        metrics.display(),
        fake.address
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, _) = common::run(folder, RUN);
    assert!(!folder.join("metrics/run.prom.tmp").exists());
    let text = std::fs::read_to_string(&metrics).expect("the metrics file");
    (text, execution.error.map(|error| error.to_string()))
}

/// The lines with the run's session and its measured times masked; uwe's test executor
/// measures no memory, so the peak RSS lines are left out.
fn normal(text: &str) -> Vec<String> {
    (text.lines())
        .filter(|line| !line.contains("nf_task_peak_rss_bytes_max"))
        .map(|line| {
            let mut line = line.replace("romantic_koch", RUN);
            if let Some(start) = line.find("session_id=\"") {
                let end = line[start + 12..]
                    .find('"')
                    .map_or(line.len(), |end| start + 12 + end);
                line.replace_range(start + 12..end, "SESSION");
            }
            let timed = ["_seconds_total{", "_duration_seconds{"];
            if timed.iter().any(|name| line.contains(name)) {
                let value = line.rfind(' ').map_or(line.len(), |space| space + 1);
                line.replace_range(value.., "X");
            }
            line
        })
        .collect()
}

#[test]
fn pushes_metrics() {
    let fake = Fake::start(json!([{"match": "/metrics/job/", "body": "", "type": "text/plain"}]));
    let (text, error) = run("main.nf", &fake);
    assert_eq!(error, None);
    assert_eq!(
        normal(&text),
        normal(include_str!("fixtures/nf_prometheus/success.prom"))
    );
    let requests = fake.requests();
    assert!(requests.len() >= 2, "the first and the last push");
    for request in &requests {
        assert_eq!(request.method, "PUT");
        assert_eq!(request.path, format!("/metrics/job/nf+job/run_name/{RUN}"));
        assert_eq!(
            request.header("Content-Type"),
            Some("text/plain; version=0.0.4; charset=utf-8")
        );
    }
    let pushes = recorded(include_str!("fixtures/nf_prometheus/req-success.log"));
    let first = pushes[0]["body"]["_text"].as_str().unwrap_or_default();
    assert_eq!(normal(&requests[0].text()), normal(first));
    assert_eq!(requests.last().map(|request| request.text()), Some(text));
}

#[test]
fn marks_failed_run() {
    let fake = Fake::start(json!([{"match": "/metrics/job/", "body": "", "type": "text/plain"}]));
    let (text, error) = run("fail.nf", &fake);
    assert!(error.is_some());
    assert_eq!(
        normal(&text),
        normal(include_str!("fixtures/nf_prometheus/fail.prom"))
    );
    assert_eq!(
        fake.requests().last().map(|request| request.text()),
        Some(text)
    );
}
