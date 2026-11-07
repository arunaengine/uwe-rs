//! nf-nextpie against the upload that nf-nextpie 0.0.2 sent with Nextflow 26.04.6 to a local
//! fake server (`req-success.log`), for the probe pipeline of the nf-tower fixtures.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;
#[path = "common/fake.rs"]
mod fake;

use std::path::Path;

use fake::{Fake, recorded};
use serde_json::json;

const SETTINGS: &str = "plugins { id 'nf-nextpie@0.0.2' }\nparams { group = 'demo-group'; \
                        name = 'demo-project' }\n";

/// A run of the probe pipeline in `folder` with this config added, where `{folder}` is the
/// folder, and the error that failed it.
fn run(folder: &Path, fake: &Fake, config: &str) -> Option<String> {
    common::copy(&common::fixtures("nf_tower"), folder, &["main.nf"]);
    let port = fake.address.rsplit(':').next().unwrap_or_default();
    let config = format!(
        "{SETTINGS}{config}nextpie {{ host = '127.0.0.1'; port = {port}; key = \
         'not-a-real-key' }}\n"
    );
    let config = config.replace("{folder}", &folder.display().to_string());
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, _) = common::run(folder, "backstabbing_ride");
    execution.error.map(|error| error.to_string())
}

/// The form fields before the file, and the file's text.
fn split(body: &str) -> (&str, &str) {
    let (fields, file) = body
        .split_once("Content-Type: text/plain\r\n\r\n")
        .expect("a file part");
    let file = file
        .strip_suffix("\r\n------WebKitFormBoundary7MA4YWxkTrZu0gW--\r\n")
        .expect("the closing boundary");
    (fields, file)
}

#[test]
fn uploads_run_trace() {
    let fake = Fake::start(json!([{
        "match": "/api/v1.0/upload-data",
        "body": {"message": "Data uploaded", "status": "success"},
    }]));
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let config = "manifest { name = 'demo/probe'; version = '1.0.0' }\ntrace { enabled = true; \
                  overwrite = true; file = '{folder}/pipeline_info/trace.txt' }\n";
    assert_eq!(run(folder, &fake, config), None);
    let requests = fake.requests();
    assert_eq!(requests.len(), 1, "one upload");
    let sent = &requests[0];
    let expected = &recorded(include_str!("fixtures/nf_nextpie/req-success.log"))[0];
    assert_eq!(
        (sent.method.as_str(), sent.path.as_str()),
        ("POST", "/api/v1.0/upload-data")
    );
    assert_eq!(
        sent.header("Content-Type"),
        expected["headers"]["Content-Type"].as_str()
    );
    assert_eq!(sent.header("X-API-KEY"), Some("not-a-real-key"));

    let text = sent.text();
    let (fields, trace) = split(&text);
    let (recorded_fields, recorded_trace) = split(expected["body"]["_text"].as_str().unwrap_or(""));
    assert_eq!(fields, recorded_fields);
    let header = |trace: &str| trace.lines().next().map(str::to_owned);
    assert_eq!(header(trace), header(recorded_trace));
    let tasks = |trace: &str| {
        let mut tasks: Vec<String> = (trace.lines().skip(1))
            .map(|line| {
                let columns: Vec<&str> = line.split('\t').collect();
                assert_eq!(columns.len(), 14, "{line}");
                format!("{} {} {}", columns[3], columns[4], columns[5])
            })
            .collect();
        tasks.sort();
        tasks
    };
    assert_eq!(tasks(trace), tasks(recorded_trace));
    for line in trace.lines().skip(1) {
        let columns: Vec<&str> = line.split('\t').collect();
        assert!(
            columns[1].len() == 9 && columns[1].as_bytes()[2] == b'/',
            "{line}"
        );
        assert!(
            columns[6].len() == 23 && columns[6].as_bytes()[10] == b' ',
            "{line}"
        );
        assert!(columns[7].ends_with('s'), "{line}");
    }
}

#[test]
fn skips_incomplete_config() {
    let fake = Fake::start(json!([]));
    let without_trace = "manifest { name = 'demo/probe'; version = '1.0.0' }\n";
    let without_name = "trace { enabled = true; file = '{folder}/trace.txt' }\n";
    for config in [without_trace, without_name] {
        let directory = tempfile::tempdir().expect("a temporary directory");
        assert_eq!(
            run(directory.path(), &fake, config),
            None,
            "the run goes on"
        );
    }
    assert!(fake.requests().is_empty());
}
