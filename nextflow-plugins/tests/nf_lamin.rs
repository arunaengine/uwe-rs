//! nf-lamin's run tracking against the requests that nf-lamin 0.9.0 sent with Nextflow 26.04.6
//! to a local fake of LaminHub and the instance API (`req-*.log`, one JSON line per request).
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;
#[path = "common/fake.rs"]
mod fake;

use fake::{Fake, Request, recorded};
use serde_json::{Value, json};

const INSTANCE: &str = "11111111-2222-3333-4444-555555555555";

/// Answers like the fake of the recordings, with fixed ids and uids.
fn routes(settings: Value) -> Value {
    json!([
        {"match": "/upsert", "body": []},
        {"match": "get-jwt-v1", "body": {"accessToken": "fake.jwt.token"}},
        {"match": "get-instance-settings-v1", "body": settings},
        {"match": "/api/account", "body": {"handle": "demo-user"}},
        {"method": "POST", "match": "/core/transform?", "body": []},
        {"match": "/transforms", "body": {"body": {"transform": {"id": 1, "uid": "TRANSFORM0001"}}}},
        {"method": "PUT", "match": "/core/run", "body": [{"id": 5, "uid": "RUN0001"}]},
        {"method": "POST", "match": "/core/project/", "body": {"id": 501, "uid": "projDEMO00000001"}},
        {"method": "POST", "match": "/core/ulabel/", "body": {"id": 601, "uid": "ulabRUN000000001"}},
        {"method": "POST", "match": "/core/ulabel?", "body": []},
        {"method": "PUT", "match": "/core/ulabel", "body": [{"id": 3, "uid": "LABEL3"}]},
        {"match": "/artifacts/create", "body": {"body": {"artifact": {"id": 9, "uid": "ARTIFACT9"}}}},
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

/// The method and path of a request, with the instance and record uids left out.
fn shape(method: &str, path: &str) -> String {
    let mut path = path.replace(INSTANCE, "<instance>");
    for model in ["run", "artifact"] {
        let marker = format!("/core/{model}/");
        if let Some(place) = path.find(&marker) {
            path.truncate(place + marker.len());
            path.push_str("<uid>");
        }
    }
    format!("{method} {path}")
}

fn shapes(requests: &[Request]) -> Vec<String> {
    (requests.iter())
        .map(|request| shape(&request.method, &request.path))
        .collect()
}

fn bodies(requests: &[Request], method: &str, part: &str) -> Vec<Value> {
    (requests.iter())
        .filter(|request| request.method == method && request.path.contains(part))
        .map(Request::json)
        .collect()
}

#[test]
fn records_run() {
    let fake = Fake::start(routes(instance()));
    let (project, error, _) = run("main.nf", &fake, "", "prickly_archimedes");
    assert_eq!(error, None);
    let requests = fake.requests();
    let expected: Vec<String> = (recorded(include_str!("fixtures/nf_lamin/req-basic.log")).iter())
        .map(|line| {
            shape(
                line["method"].as_str().unwrap_or_default(),
                line["path"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(shapes(&requests), expected);
    assert_eq!(requests[0].json(), json!({"api_key": "not-a-real-key"}));
    assert_eq!(
        requests[0].header("Authorization"),
        Some("Bearer fake-anon-key")
    );
    assert_eq!(
        requests[2].header("Authorization"),
        Some("Bearer fake.jwt.token")
    );
    let filter = json!({"filter": {"and": [{"key": {"eq": project}},
                                            {"version_tag": {"eq": "local-development"}}]}});
    assert_eq!(requests[3].json(), filter);
    let transform = requests[4].json();
    assert_eq!(
        transform["source_code"],
        format!("repo: {project}\npath: main.nf")
    );
    assert_eq!(
        transform["kwargs"],
        json!({"reference": project, "version_tag": "local-development",
               "description": "demo/probe: probe", "reference_type": "url"})
    );
    let created = requests[5].json();
    assert_eq!(
        (created["transform_id"].clone(), created["name"].clone()),
        (json!(1), json!("prickly_archimedes"))
    );
    assert_eq!(created["_status_code"], -3);
    let time = created["created_at"].as_str().unwrap_or_default();
    assert!(time.len() == 32 && time.ends_with("+00:00"), "{time}");
    assert_eq!(requests[6].json()["_status_code"], -1);
    assert_eq!(requests[7].json()["_status_code"], 0);
    assert!(requests[7].path.ends_with("/core/run/RUN0001"));
}

#[test]
fn marks_failed_run() {
    let fake = Fake::start(routes(instance()));
    let (_, error, _) = run("fail.nf", &fake, "", "failing_run");
    assert!(error.is_some());
    let ended = bodies(&fake.requests(), "PATCH", "/core/run/");
    assert_eq!(
        ended.last().map(|body| body["_status_code"].clone()),
        Some(json!(1))
    );
}

#[test]
fn links_files() {
    let fake = Fake::start(routes(instance()));
    let extra = "  project_uids = ['projDEMO00000001']\n  ulabel_uids = ['+nextflow']\n  \
                 run { ulabel_uids = ['ulabRUN000000001'] }\n  input_artifacts {\n    rules { \
                 sheet { include_paths = ['s3://demo-bucket/inputs/sheet.csv']; kind = 'dataset' \
                 } }\n  }\n  output_artifacts {\n    include_paths = \
                 ['s3://demo-bucket/results/out.txt', 's3://demo-bucket/results/skip.log']\n    \
                 rules { logs { type = 'exclude'; pattern = '.*\\\\.log$' } }\n  }\n";
    let (_, error, printed) = run("main.nf", &fake, extra, "special_varahamihira");
    assert_eq!(error, None, "{printed}");
    let requests = fake.requests();
    let upserts = |model: &str| bodies(&requests, "PUT", &format!("/core/{model}/upsert"));
    assert_eq!(
        upserts("transformproject"),
        [json!([{"transform_id": 1, "project_id": 501}])]
    );
    assert_eq!(
        upserts("transformulabel"),
        [json!([{"transform_id": 1, "ulabel_id": 3}])]
    );
    assert_eq!(
        upserts("runulabel"),
        [json!([{"run_id": 5, "ulabel_id": 3}, {"run_id": 5, "ulabel_id": 601}])]
    );
    let made = bodies(&requests, "POST", "/artifacts/create");
    assert_eq!(
        made[0]["kwargs"],
        json!({"kind": "dataset",
               "description": "Input artifact at s3:///demo-bucket/inputs/sheet.csv"})
    );
    let outputs: Vec<&Value> = made[1..].iter().map(|body| &body["path"]).collect();
    assert_eq!(
        outputs,
        [
            "s3://demo-bucket/results/out.txt",
            "s3://demo-bucket/results/skip.log"
        ]
    );
    assert_eq!(
        made[1]["kwargs"],
        json!({"run_id": 5, "description": "Output artifact for run 5"})
    );
    assert_eq!(
        upserts("artifact_input_of_runs"),
        [json!([{"artifact_id": 9, "run_id": 5}])]
    );
    let ended = bodies(&requests, "PATCH", "/core/run/RUN0001");
    assert_eq!(
        ended.last().map(|body| body["_status_code"].clone()),
        Some(json!(0))
    );
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
