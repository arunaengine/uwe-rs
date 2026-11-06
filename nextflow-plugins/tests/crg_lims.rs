//! nf-crg-genomics-lims against the requests that nf-crg-genomics-lims 0.1.1 sent with Nextflow
//! 26.04.6 to a local fake server (`req-*.log`), for the probe pipelines `main.nf` and `fail.nf`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;
#[path = "common/fake.rs"]
mod fake;

use fake::{Fake, Request, recorded};
use serde_json::json;

/// A run of `script` with the plugin and these params, with the requests it sent.
fn run(script: &str, params: &str, name: &str) -> (bool, Vec<Request>) {
    let fake = Fake::start(json!([{"match": "/api/", "body": {"ok": true}}]));
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let text = std::fs::read_to_string(common::fixtures("crg_lims").join(script))
        .expect("reads the probe");
    let results = format!("publishDir '{}/results'", folder.display());
    let text = text.replace("publishDir 'results'", &results);
    std::fs::write(folder.join("main.nf"), text).expect("writes the probe");
    let params = params.replace("{folder}", &folder.to_string_lossy());
    let config = format!(
        "plugins {{ id 'nf-crg-genomics-lims@0.1.1' }}\nlims {{ enabled = true }}\nparams {{\n  \
         lims_api_base_url = '{}/api/executions'\n  lims_pipeline_execution_id = '12345'\n  \
         lims_username = 'demo-user'\n  lims_api_key = 'not-a-real-key'\n{params}}}\n",
        fake.address
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, _) = common::run(folder, name);
    (execution.error.is_none(), fake.requests())
}

#[test]
fn reports_success() {
    let params = "  lims_upload_file = '{folder}/results/report.html'\n";
    let (succeeded, requests) = run("main.nf", params, "lims_success");
    assert!(succeeded);
    let expected = recorded(include_str!("fixtures/crg_lims/req-success.log"));
    let shapes: Vec<(&str, &str)> = (requests.iter())
        .map(|request| (request.method.as_str(), request.path.as_str()))
        .collect();
    let recorded: Vec<(&str, &str)> = (expected.iter())
        .map(|line| {
            (
                line["method"].as_str().unwrap_or_default(),
                line["path"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(shapes, recorded);
    assert_eq!(requests[0].json(), expected[0]["body"]);
    for request in &requests {
        assert_eq!(
            request.header("Authorization"),
            Some("ApiKey demo-user:not-a-real-key")
        );
    }
    let kind = requests[1].header("Content-Type").unwrap_or_default();
    let boundary = kind
        .strip_prefix("multipart/form-data; boundary=")
        .expect("a multipart body");
    assert!(boundary.starts_with("NextflowLimsBoundary"), "{boundary}");
    let text = expected[1]["body"]["_text"].as_str().unwrap_or_default();
    let recorded_boundary = text
        .lines()
        .next()
        .unwrap_or_default()
        .trim_start_matches("--");
    assert_eq!(
        requests[1].text(),
        text.replace(recorded_boundary, boundary)
    );
}

#[test]
fn reports_failure_once() {
    let params = "  lims_upload_file = '{folder}/main.nf'\n  lims_upload_mode = 'binary'\n  \
                  lims_upload_method = 'put'\n  lims_status_key = 'state'\n";
    let (succeeded, requests) = run("fail.nf", params, "lims_failure");
    assert!(!succeeded);
    let expected = recorded(include_str!("fixtures/crg_lims/req-fail.log"));
    assert_eq!(requests.len(), 2, "one status update and one upload");
    assert_eq!(
        (requests[0].method.as_str(), requests[0].json()),
        ("PATCH", expected[0]["body"].clone())
    );
    assert_eq!(
        (requests[1].method.as_str(), requests[1].path.as_str()),
        ("PUT", "/api/executions/12345/upload")
    );
    assert_eq!(
        requests[1].header("Content-Type"),
        Some("application/octet-stream")
    );
    let script =
        std::fs::read_to_string(common::fixtures("crg_lims").join("fail.nf")).expect("reads");
    assert_eq!(requests[1].text(), script);
}

#[test]
fn skips_without_address() {
    let fake = Fake::start(json!([]));
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    common::copy(&common::fixtures("crg_lims"), folder, &["fail.nf"]);
    std::fs::rename(folder.join("fail.nf"), folder.join("main.nf")).expect("renames");
    let config = "plugins { id 'nf-crg-genomics-lims@0.1.1' }\nparams { lims_enabled = true }\n";
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, _) = common::run(folder, "lims_skipped");
    assert!(execution.error.is_some());
    assert!(fake.requests().is_empty());
}
