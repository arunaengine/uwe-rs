//! The lakeFS provider against a local fake of the lakeFS API: stat, list and presigned reads,
//! staged writes with a new branch, and commits.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "common/service.rs"]
mod service;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use config_file::ConfigFile;
use serde_json::{Value, json};
use service::{Reply, Request, Service};
use storage::{Entry, Storage};

/// `Basic` with the user `AKIAFAKELOCALTEST` and the password `fake-secret`.
const AUTHORIZATION: &str = "Basic QUtJQUZBS0VMT0NBTFRFU1Q6ZmFrZS1zZWNyZXQ=";

#[derive(Default)]
struct Repository {
    branches: HashSet<String>,
    /// The blob of each path of a branch.
    objects: HashMap<(String, String), String>,
    blobs: HashMap<String, Vec<u8>>,
    commits: u32,
}

/// Repository `repo1` with branch `main`, which holds `data/in.txt` and `data/sub/b.csv`.
/// Presigned addresses lead back to the fake under `/blob/`.
fn lakefs() -> Service {
    let mut repository = Repository::default();
    repository.branches.insert("main".into());
    for (path, blob, bytes) in [
        ("data/in.txt", "in", "hello lakefs\n"),
        ("data/sub/b.csv", "b", "x,y\n1,2\n"),
    ] {
        repository
            .objects
            .insert(("main".into(), path.into()), blob.into());
        repository
            .blobs
            .insert(blob.into(), bytes.as_bytes().to_vec());
    }
    let state = Arc::new(Mutex::new(repository));
    Service::start(move |request: &Request, base: &str| {
        let (path, query) = request.path.split_once('?').unwrap_or((&request.path, ""));
        let query: HashMap<&str, String> = (query.split('&'))
            .filter_map(|pair| pair.split_once('='))
            .map(|(key, value)| (key, value.replace("%2F", "/")))
            .collect();
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        let mut state = state.lock().expect("the repository");
        let state = &mut *state;
        if let ["blob", blob] = parts.as_slice() {
            if request.headers.contains_key("authorization") {
                return Reply::json(400, json!({ "message": "a presigned request has no auth" }));
            }
            return match request.method.as_str() {
                "PUT" => {
                    state.blobs.insert((*blob).into(), request.body.clone());
                    Reply {
                        status: 200,
                        headers: vec![("ETag".into(), "\"etag-up\"".into())],
                        body: Vec::new(),
                    }
                }
                _ => Reply {
                    status: 200,
                    headers: Vec::new(),
                    body: state.blobs.get(*blob).cloned().unwrap_or_default(),
                },
            };
        }
        if request.headers.get("authorization").map(String::as_str) != Some(AUTHORIZATION) {
            return Reply::json(401, json!({ "message": "unauthorized" }));
        }
        let stat = |branch: &str, path: &str, blob: &str, presign: bool| {
            let size = state.blobs.get(blob).map_or(0, Vec::len);
            let address = match presign {
                true => format!("{base}/blob/{blob}"),
                false => format!("s3://lakefs-bucket/repo1/{blob}"),
            };
            json!({ "path": path, "path_type": "object", "physical_address": address,
                "checksum": format!("sum-{blob}"), "size_bytes": size, "mtime": 1767225600,
                "branch": branch })
        };
        let presign = query.get("presign").is_some_and(|value| value == "true");
        let object = |branch: &str| {
            query
                .get("path")
                .map(|path| (branch.to_owned(), path.clone()))
        };
        match (request.method.as_str(), parts.as_slice()) {
            ("POST", ["api", "v1", "repositories", "repo1", "branches"]) => {
                let body: Value = serde_json::from_slice(&request.body).unwrap_or_default();
                let name = body["name"].as_str().unwrap_or_default().to_owned();
                for ((branch, path), blob) in state.objects.clone() {
                    if Some(branch.as_str()) == body["source"].as_str() {
                        state.objects.insert((name.clone(), path), blob);
                    }
                }
                state.branches.insert(name);
                Reply::json(201, json!("c-new"))
            }
            ("GET", ["api", "v1", "repositories", "repo1", "branches", branch]) => {
                match state.branches.contains(*branch) {
                    true => Reply::json(200, json!({ "id": branch, "commit_id": "c0" })),
                    false => Reply::json(404, json!({ "message": "branch not found" })),
                }
            }
            (
                "POST",
                [
                    "api",
                    "v1",
                    "repositories",
                    "repo1",
                    "branches",
                    _,
                    "commits",
                ],
            ) => {
                state.commits += 1;
                Reply::json(201, json!({ "id": format!("c{}", state.commits) }))
            }
            (
                "GET",
                [
                    "api",
                    "v1",
                    "repositories",
                    "repo1",
                    "branches",
                    branch,
                    "staging",
                    "backing",
                ],
            ) => {
                let blob = format!("up-{branch}");
                Reply::json(
                    200,
                    json!({ "physical_address": format!("s3://lakefs-bucket/repo1/{blob}"),
                    "presigned_url": format!("{base}/blob/{blob}"), "presigned_url_expiry": 1 }),
                )
            }
            (
                "PUT",
                [
                    "api",
                    "v1",
                    "repositories",
                    "repo1",
                    "branches",
                    branch,
                    "staging",
                    "backing",
                ],
            ) => {
                let key = object(branch).expect("a path");
                let blob = format!("up-{branch}");
                state.objects.insert(key.clone(), blob.clone());
                Reply::json(200, stat(branch, &key.1, &blob, false))
            }
            (
                "GET",
                [
                    "api",
                    "v1",
                    "repositories",
                    "repo1",
                    "refs",
                    reference,
                    "objects",
                    "stat",
                ],
            ) => {
                let key = object(reference).expect("a path");
                match state.objects.get(&key) {
                    Some(blob) => Reply::json(200, stat(reference, &key.1, blob, presign)),
                    None => Reply::json(404, json!({ "message": "object not found" })),
                }
            }
            (
                "GET",
                [
                    "api",
                    "v1",
                    "repositories",
                    "repo1",
                    "refs",
                    reference,
                    "objects",
                    "ls",
                ],
            ) => {
                if !state.branches.contains(*reference) {
                    return Reply::json(404, json!({ "message": "ref not found" }));
                }
                let prefix = query.get("prefix").cloned().unwrap_or_default();
                let mut results = Vec::new();
                let mut seen = HashSet::new();
                let mut paths: Vec<_> = state.objects.iter().collect();
                paths.sort();
                for ((branch, path), blob) in paths {
                    if branch != reference || !path.starts_with(&prefix) {
                        continue;
                    }
                    match path[prefix.len()..].split_once('/') {
                        Some((folder, _)) if seen.insert(folder.to_owned()) => results.push(
                            json!({ "path": format!("{prefix}{folder}/"), "path_type": "common_prefix" }),
                        ),
                        Some(_) => {}
                        None => results.push(stat(branch, path, blob, false)),
                    }
                }
                Reply::json(
                    200,
                    json!({ "results": results,
                    "pagination": { "has_more": false, "next_offset": "" } }),
                )
            }
            _ => Reply::json(404, json!({ "message": "unknown" })),
        }
    })
}

fn storage(service: &Service) -> Storage {
    let text = format!(
        "[storage.lake]\ntype = \"lakefs\"\nprefixes = [\"lakefs://\"]\nendpoint = \"{}/api/v1\"\n\
         access_key = \"AKIAFAKELOCALTEST\"\nsecret_key = \"fake-secret\"\n\
         create_branches = true\n",
        service.address
    );
    let config = ConfigFile::parse(&text, Path::new("/home/me/.config/uwe/config.toml"));
    Storage::from_config(&config.expect("TOML")).expect("valid settings")
}

#[tokio::test]
async fn reads_through_presigned() {
    let service = lakefs();
    let storage = storage(&service);
    let file = storage
        .stat("lakefs://repo1/main/data/in.txt")
        .await
        .expect("a stat");
    let file = file.expect("the object");
    assert_eq!((file.directory, file.size), (false, 13));
    assert_eq!(file.version.as_deref(), Some("sum-in"));
    let folder = storage
        .stat("lakefs://repo1/main/data")
        .await
        .expect("a stat");
    assert!(folder.expect("a prefix").directory);
    let missing = storage.stat("lakefs://repo1/main/data/none.txt").await;
    assert_eq!(missing.expect("a stat"), None);
    let entries = storage
        .list("lakefs://repo1/main/data")
        .await
        .expect("a listing");
    let expected = [("in.txt", false), ("sub", true)].map(|(name, directory)| Entry {
        name: name.into(),
        directory,
    });
    assert_eq!(entries, expected);
    let downloads = tempfile::tempdir().expect("a folder");
    let copy = storage
        .fetch("lakefs://repo1/main/data/in.txt", downloads.path())
        .await;
    let text = std::fs::read_to_string(copy.expect("a copy")).expect("text");
    assert_eq!(text, "hello lakefs\n");
    let lines = service.lines();
    let presigned =
        "GET /api/v1/repositories/repo1/refs/main/objects/stat?path=data/in.txt&presign=true";
    assert!(lines.contains(&presigned.to_owned()), "{lines:?}");
    assert!(lines.contains(&"GET /blob/in".to_owned()), "{lines:?}");
}

#[tokio::test]
async fn stages_new_branch() {
    let service = lakefs();
    let storage = storage(&service);
    let folder = tempfile::tempdir().expect("a folder");
    let file = folder.path().join("copy_in.txt");
    std::fs::write(&file, "hello lakefs\n").expect("a file");
    let target = "lakefs://repo1/out/results/copy_in.txt";
    let receipt = storage
        .upload_with_receipt(&file, target)
        .await
        .expect("an upload");
    assert_eq!(
        (receipt.size, receipt.version.as_deref()),
        (13, Some("sum-up-out"))
    );
    let requests = service.requests.lock().expect("the requests").clone();
    let lines: Vec<String> = (requests.iter())
        .map(|request| format!("{} {}", request.method, request.path))
        .collect();
    let staging =
        "/api/v1/repositories/repo1/branches/out/staging/backing?path=results/copy_in.txt";
    assert_eq!(
        lines,
        [
            "GET /api/v1/repositories/repo1/branches/out".to_owned(),
            "POST /api/v1/repositories/repo1/branches".into(),
            format!("GET {staging}&presign=true"),
            "PUT /blob/up-out".into(),
            format!("PUT {staging}"),
        ]
    );
    let made: Value = serde_json::from_slice(&requests[1].body).expect("JSON");
    assert_eq!(
        made,
        json!({ "name": "out", "source": "main", "force": false, "hidden": false })
    );
    assert_eq!(requests[3].body, b"hello lakefs\n");
    let link: Value = serde_json::from_slice(&requests[4].body).expect("JSON");
    assert_eq!(
        (&link["size_bytes"], &link["checksum"]),
        (&json!(13), &json!("etag-up"))
    );
    assert_eq!(
        link["staging"]["presigned_url"],
        json!(format!("{}/blob/up-out", service.address))
    );
    storage
        .upload(&file, target)
        .await
        .expect("a second upload");
    let branch_checks = (service.lines().iter())
        .filter(|line| line.ends_with("/branches/out"))
        .count();
    assert_eq!(branch_checks, 1, "a known branch is not asked for again");
}

#[test]
fn commits_branch() {
    let service = lakefs();
    storage::lakefs::use_scope(storage::lakefs::Options {
        endpoint: Some(format!("{}/api/v1", service.address)),
        access_key: Some("AKIAFAKELOCALTEST".into()),
        secret_key: Some("fake-secret".into()),
        ..storage::lakefs::Options::default()
    });
    let id = storage::lakefs::commit("lakefs://repo1/main/anything/here", "probe commit");
    assert_eq!(id.expect("a commit"), "c1");
    let requests = service.requests.lock().expect("the requests").clone();
    let commit = requests.last().expect("a request");
    assert_eq!(
        commit.path,
        "/api/v1/repositories/repo1/branches/main/commits"
    );
    let body: Value = serde_json::from_slice(&commit.body).expect("JSON");
    assert_eq!(
        body,
        json!({ "message": "probe commit", "metadata": {}, "allow_empty": false, "force": false })
    );
    let missing =
        storage::lakefs::commit("lakefs://repo1/none/x", "message").expect_err("no branch");
    assert!(
        missing.to_string().contains("'none' does not exist"),
        "{missing}"
    );
    let refused = storage::lakefs::create_branch("lakefs://repo1/new/x").expect_err("not allowed");
    assert!(
        refused.to_string().contains("autoCreateBranch"),
        "{refused}"
    );
    assert!(!refused.to_string().contains("fake-secret"), "{refused}");
}
