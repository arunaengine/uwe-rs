//! The LaminDB provider against a local fake of LaminHub and an instance API: artifacts
//! resolve to files below the roots of their storages, which the fake serves too.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "common/service.rs"]
mod service;

use std::path::Path;

use config_file::ConfigFile;
use serde_json::{Value, json};
use service::{Reply, Request, Service};
use storage::Storage;

const INSTANCE: &str = "11111111-2222-3333-4444-555555555555";
const SHEET: &str = "AbCdEfGhIjKlMnOp0000";

/// Instance `demo-org/demo-inst`, whose default storage is the folder `/root` of the fake and
/// a second one `/other`; the artifacts are a sample sheet, a folder and an unnamed table.
fn lamin() -> Service {
    Service::start(move |request: &Request, base: &str| {
        let path = request.path.split('?').next().unwrap_or_default();
        let body: Value = serde_json::from_slice(&request.body).unwrap_or_default();
        let bearer = request.headers.get("authorization").map(String::as_str);
        match path {
            "/functions/v1/get-jwt-v1" if bearer == Some("Bearer anon-key") => {
                Reply::json(200, json!({ "accessToken": "fake.jwt.token" }))
            }
            "/root/inputs/sheet.csv" if bearer.is_none() => text("sample,fastq\n"),
            "/other/.lamindb/TaBlE000000000000000.parquet" if bearer.is_none() => text("PAR1"),
            _ if bearer != Some("Bearer fake.jwt.token") => {
                Reply::json(401, json!({ "message": "unauthorized" }))
            }
            "/functions/v1/get-instance-settings-v1" if body["name"] == "demo-inst" => Reply::json(
                200,
                json!({ "id": INSTANCE, "owner": "demo-org", "name": "demo-inst",
                    "schema_id": "schema-1", "api_url": format!("{base}/api"),
                    "storage": { "id": "storage-1", "root": format!("{base}/root"),
                        "type": "http", "region": "us-east-1" } }),
            ),
            "/functions/v1/get-instance-settings-v1" => Reply::json(200, json!({})),
            _ => match path.strip_prefix(&format!("/api/instances/{INSTANCE}/modules/core/")) {
                Some(record) => match record.split_once('/') {
                    Some(("artifact", SHEET)) => Reply::json(
                        200,
                        json!({ "uid": SHEET, "key": "inputs/sheet.csv", "suffix": ".csv",
                            "size": 13, "hash": "hash-sheet", "storage_id": "storage-1" }),
                    ),
                    Some(("artifact", "FoLdEr00000000000000")) => Reply::json(
                        200,
                        json!({ "uid": "FoLdEr00000000000000", "key": "results", "suffix": "",
                            "n_files": 2, "storage_id": "storage-1" }),
                    ),
                    Some(("artifact", "TaBlE000000000000000")) => Reply::json(
                        200,
                        json!({ "uid": "TaBlE000000000000000", "key": null, "suffix": ".parquet",
                            "size": 4, "storage_id": 7 }),
                    ),
                    Some(("storage", "7")) => {
                        Reply::json(200, json!({ "id": 7, "root": format!("{base}/other") }))
                    }
                    _ => Reply::json(404, json!({ "detail": "not found" })),
                },
                None => Reply::json(404, json!({ "detail": "unknown path" })),
            },
        }
    })
}

fn text(body: &str) -> Reply {
    Reply {
        status: 200,
        headers: vec![("Content-Type".into(), "text/plain".into())],
        body: body.as_bytes().to_vec(),
    }
}

fn storage(service: &Service) -> Storage {
    let text = format!(
        "[storage.lamin]\ntype = \"lamin\"\nprefixes = [\"lamin://\"]\nhub = \"{}\"\n\
         api_key = \"not-a-real-key\"\nanon_key = \"anon-key\"\n",
        service.address
    );
    let config = ConfigFile::parse(&text, Path::new("/home/me/.config/uwe/config.toml"));
    Storage::from_config(&config.expect("TOML")).expect("valid settings")
}

#[tokio::test]
async fn resolves_artifacts() {
    let service = lamin();
    let storage = storage(&service);
    let sheet = format!("lamin://demo-org/demo-inst/artifact/{SHEET}");
    let found = storage
        .stat(&sheet)
        .await
        .expect("a stat")
        .expect("the artifact");
    assert_eq!((found.directory, found.size), (false, 13));
    assert_eq!(found.version.as_deref(), Some("hash-sheet"));
    let folder = storage
        .stat("lamin://demo-org/demo-inst/artifact/FoLdEr00000000000000")
        .await;
    assert!(folder.expect("a stat").expect("the artifact").directory);
    let missing = storage
        .stat("lamin://demo-org/demo-inst/artifact/NoNe0000000000000000")
        .await;
    assert_eq!(missing.expect("a stat"), None);
    let downloads = tempfile::tempdir().expect("a folder");
    let copy = storage
        .fetch(&sheet, downloads.path())
        .await
        .expect("a copy");
    assert_eq!(
        std::fs::read_to_string(copy).expect("text"),
        "sample,fastq\n"
    );
    let table = "lamin://demo-org/demo-inst/artifact/TaBlE000000000000000";
    let copy = storage
        .fetch(table, downloads.path())
        .await
        .expect("a copy");
    assert_eq!(std::fs::read_to_string(copy).expect("text"), "PAR1");
    let lines = service.lines();
    let settings = (lines.iter())
        .filter(|line| line.ends_with("get-instance-settings-v1"))
        .count();
    let tokens = lines
        .iter()
        .filter(|line| line.ends_with("get-jwt-v1"))
        .count();
    assert_eq!((settings, tokens), (1, 1), "{lines:?}");
    let record = format!(
        "POST /api/instances/{INSTANCE}/modules/core/artifact/{SHEET}?schema_id=schema-1&\
         include_foreign_keys=true"
    );
    assert!(lines.contains(&record), "{lines:?}");
    let requests = service.requests.lock().expect("the requests").clone();
    let jwt: Value = serde_json::from_slice(&requests[0].body).expect("JSON");
    assert_eq!(jwt, json!({ "api_key": "not-a-real-key" }));
}

#[tokio::test]
async fn names_unknown_instance() {
    let service = lamin();
    let storage = storage(&service);
    let error = storage
        .stat("lamin://demo-org/other/artifact/x")
        .await
        .expect_err("no such instance");
    let message = error.to_string();
    assert!(
        message.contains("LaminDB instance demo-org/other could not be found."),
        "{message}"
    );
    assert!(!message.contains("not-a-real-key"), "{message}");
}
