//! nf-lamin's run tracking against the requests that nf-lamin 0.9.0 sent with Nextflow 26.04.6
//! to a local fake of LaminHub and the instance API (`req-*.log`, one JSON line per request).
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;
#[path = "common/fake.rs"]
mod fake;

use fake::Fake;
use serde_json::{Value, json};

const INSTANCE: &str = "11111111-2222-3333-4444-555555555555";

/// Answers like the fake of the recordings.
fn routes(settings: Value) -> Value {
    json!([
        {"match": "get-jwt-v1", "body": {"accessToken": "fake.jwt.token"}},
        {"match": "get-instance-settings-v1", "body": settings},
        {"match": "/api/account", "body": {"handle": "demo-user"}},
        {"match": "/", "body": {}}
    ])
}

fn instance() -> Value {
    json!({"id": INSTANCE, "owner": "demo-org", "name": "demo-inst",
           "api_url": "{address}/api", "storage": {"root": "s3://demo-bucket"}})
}

/// A run of `script` with nf-lamin and the extra `lamin` settings, with its project name, its
/// error and what it printed.
fn run(script: &str, fake: &Fake, extra: &str, name: &str) -> (String, Option<String>, String) {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let text = std::fs::read_to_string(common::fixtures("nf_lamin").join(script))
        .expect("reads the script");
    let results = format!("publishDir '{}/results'", folder.display());
    let text = text.replace("publishDir 'results'", &results);
    std::fs::write(folder.join("main.nf"), text).expect("writes the script");
    let config = format!(
        "plugins {{ id 'nf-lamin@0.9.0' }}\nmanifest {{ name = 'demo/probe'; version = '1.0.0'; \
         description = 'probe' }}\nlamin {{\n  instance = 'demo-org/demo-inst'\n  \
         api_key = 'not-a-real-key'\n  api {{ supabase_api_url = '{}'; supabase_anon_key = \
         'fake-anon-key'; max_retries = 1 }}\n{extra}}}\n",
        fake.address
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, printed) = common::run(folder, name);
    let name = folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    (
        name.unwrap_or_default(),
        execution.error.map(|error| error.to_string()),
        printed,
    )
}

#[test]
fn dry_run_connects() {
    let fake = Fake::start(routes(instance()));
    let (_, error, _) = run("main.nf", &fake, "  dry_run = true\n", "dry_run");
    assert_eq!(error, None);
    let paths: Vec<String> = fake
        .requests()
        .iter()
        .map(|request| request.path.clone())
        .collect();
    assert_eq!(
        paths,
        [
            "/functions/v1/get-jwt-v1",
            "/functions/v1/get-instance-settings-v1",
            "/api/account"
        ]
    );
}

#[test]
fn refused_instance_stops() {
    let fake = Fake::start(routes(json!({"code": "PGRST116", "message": "no access"})));
    let (_, error, _) = run("main.nf", &fake, "", "refused_run");
    let error = error.expect("the run fails");
    assert!(
        error.contains(
            "Could not initialize Lamin run: Failed to get instance settings for \
             demo-org/demo-inst: no access (code: PGRST116)"
        ),
        "{error}"
    );
    assert_eq!(fake.requests().len(), 2);
}
