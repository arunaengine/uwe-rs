//! nf-lakefs: `lakefsCommit` and `autoCommit` commit through the `lakefs` scope, against a local
//! fake of the lakeFS API, with the requests of the recorded runs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;
#[path = "common/fake.rs"]
mod fake;

use fake::Fake;
use serde_json::json;

#[test]
fn commits_output_branch() {
    let fake = Fake::start(json!([
        { "method": "POST", "match": "/commits", "status": 201, "body": { "id": "c1" } },
        { "method": "GET", "match": "/branches/out", "body": { "id": "out", "commit_id": "c0" } },
    ]));
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let config = format!(
        "plugins {{ id 'nf-lakefs@0.7.0' }}\nlakefs {{\n    apiUrl = '{}/api/v1'\n    \
         accessKey = 'AKIAFAKELOCALTEST'\n    secretKey = 'fake-secret'\n    \
         autoCreateBranch = true\n    autoCommit = true\n}}\n\
         outputDir = 'lakefs://repo1/out/results'\nparams.lakefs_commit_message = 'probe commit'\n",
        fake.address
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let script = "include { lakefsCommit } from 'plugin/nf-lakefs'\n\nworkflow {\n    \
                  println \"commit=${lakefsCommit('lakefs://repo1/out/anything/here', 'manual message')}\"\n}\n";
    std::fs::write(folder.join("main.nf"), script).expect("writes the script");
    let (execution, printed) = common::run(folder, "probe");
    assert!(execution.error.is_none(), "{:?}", execution.error);
    assert!(printed.contains("commit=c1"), "{printed}");
    let requests = fake.requests();
    let lines: Vec<String> = (requests.iter())
        .map(|request| format!("{} {}", request.method, request.path))
        .collect();
    let commit = "POST /api/v1/repositories/repo1/branches/out/commits";
    assert_eq!(
        lines.iter().filter(|line| *line == commit).count(),
        2,
        "{lines:?}"
    );
    let messages: Vec<_> = (requests.iter())
        .filter(|request| request.path.ends_with("/commits"))
        .map(|request| request.json()["message"].clone())
        .collect();
    assert_eq!(messages, [json!("manual message"), json!("probe commit")]);
    for request in &requests {
        let user = request.header("authorization");
        assert_eq!(user, Some("Basic QUtJQUZBS0VMT0NBTFRFU1Q6ZmFrZS1zZWNyZXQ="));
    }
}
