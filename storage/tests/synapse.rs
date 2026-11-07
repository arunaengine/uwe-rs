//! The Synapse provider against a local fake of the Synapse REST API: entities and versions,
//! presigned downloads, and publishing with new folders, multipart uploads and new versions.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "common/service.rs"]
mod service;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use config_file::ConfigFile;
use serde_json::{Value, json};
use service::{Reply, Request, Service};
use storage::Storage;

const CONTENT: &str = "hello from synapse\n";
const FILE: &str = "org.sagebionetworks.repo.model.FileEntity";
const FOLDER: &str = "org.sagebionetworks.repo.model.Folder";

fn storage(service: &Service) -> Storage {
    let text = format!(
        "[storage.synapse]\ntype = \"synapse\"\nprefixes = [\"syn://\"]\nendpoint = \"{}\"\n\
         token = \"fake-token\"\n",
        service.address
    );
    let config = ConfigFile::parse(&text, Path::new("/home/me/.config/uwe/config.toml"));
    Storage::from_config(&config.expect("TOML")).expect("valid settings")
}

/// The fake: file `syn100` (`hello.txt`, file handle 9001) and folder `syn200`; new entities
/// get ids from `syn301` on.
fn synapse() -> Service {
    let entities: HashMap<String, Value> = HashMap::from([
        (
            "syn100".into(),
            json!({ "id": "syn100", "name": "hello.txt", "concreteType": FILE,
                "dataFileHandleId": "9001", "parentId": "syn1", "etag": "etag-100",
                "versionNumber": 2, "fileSize": CONTENT.len(),
                "modifiedOn": "2026-01-03T03:04:05.000Z" }),
        ),
        (
            "syn200".into(),
            json!({ "id": "syn200", "name": "results", "concreteType": FOLDER,
                "parentId": "syn1", "etag": "etag-200" }),
        ),
    ]);
    let state = Arc::new(Mutex::new((entities, 300)));
    Service::start(move |request: &Request, base: &str| {
        let (path, _) = request.path.split_once('?').unwrap_or((&request.path, ""));
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        let body: Value = serde_json::from_slice(&request.body).unwrap_or_default();
        let mut state = state.lock().expect("the entities");
        let (entities, next) = &mut *state;
        match (request.method.as_str(), parts.as_slice()) {
            ("GET", ["repo", "v1", "entity", id, ..]) => match entities.get(*id) {
                Some(entity) => Reply::json(200, entity.clone()),
                None => Reply::json(404, json!({ "reason": "not found" })),
            },
            ("GET", ["file", "v1", "file", _]) => Reply {
                status: 200,
                headers: Vec::new(),
                body: format!("{base}/bucket/hello.txt?signature=x").into_bytes(),
            },
            ("GET", ["bucket", ..]) => Reply {
                status: 200,
                headers: Vec::new(),
                body: CONTENT.as_bytes().to_vec(),
            },
            ("POST", ["repo", "v1", "entity", "children"]) => {
                let page: Vec<Value> = (entities.values())
                    .filter(|entity| entity["parentId"] == body["parentId"])
                    .map(|entity| {
                        json!({ "id": entity["id"], "name": entity["name"],
                            "type": entity["concreteType"] })
                    })
                    .collect();
                Reply::json(200, json!({ "page": page }))
            }
            ("POST", ["repo", "v1", "entity"]) => {
                *next += 1;
                let id = format!("syn{next}");
                let mut entity = body.clone();
                entity["id"] = id.clone().into();
                entity["etag"] = format!("etag-{id}").into();
                entities.insert(id, entity.clone());
                Reply::json(201, entity)
            }
            ("PUT", ["repo", "v1", "entity", id]) => {
                let entity = entities.get_mut(*id).expect("an entity");
                for (key, value) in body.as_object().expect("an entity") {
                    entity[key] = value.clone();
                }
                Reply::json(200, entity.clone())
            }
            ("POST", ["file", "v1", "file", "multipart"]) => {
                Reply::json(201, json!({ "uploadId": "upload-1", "state": "UPLOADING" }))
            }
            (
                "POST",
                [
                    "file",
                    "v1",
                    "file",
                    "multipart",
                    upload,
                    "presigned",
                    "url",
                    "batch",
                ],
            ) => {
                let url = format!("{base}/bucket-put/{upload}/{}", body["partNumbers"][0]);
                Reply::json(
                    200,
                    json!({ "partPresignedUrls": [{ "partNumber": body["partNumbers"][0],
                        "uploadPresignedUrl": url,
                        "signedHeaders": { "Content-Type": "application/octet-stream" } }] }),
                )
            }
            ("PUT", ["bucket-put", ..]) => Reply::json(200, Value::Null),
            ("PUT", [.., "add", _]) => Reply::json(200, json!({ "addPartState": "ADD_SUCCESS" })),
            ("PUT", [.., "complete"]) => Reply::json(
                200,
                json!({ "state": "COMPLETED", "resultFileHandleId": "9100" }),
            ),
            _ => Reply::json(404, json!({ "reason": "unknown" })),
        }
    })
}

#[tokio::test]
async fn reads_entities() {
    let service = synapse();
    let storage = storage(&service);
    let file = storage.stat("syn://syn100.fastq.gz").await.expect("a stat");
    let file = file.expect("an entity");
    assert_eq!((file.directory, file.size), (false, CONTENT.len() as u64));
    assert_eq!(file.version.as_deref(), Some("9001"));
    let folder = storage.stat("syn://syn200").await.expect("a stat");
    assert!(folder.expect("an entity").directory);
    assert!(
        storage
            .stat("syn://syn999")
            .await
            .expect("a stat")
            .is_none()
    );
    let target = "syn://syn200/sub/out.txt";
    assert!(storage.stat(target).await.expect("a stat").is_none());
    storage.stat("syn://syn100.2").await.expect("a stat");
    let downloads = tempfile::tempdir().expect("a folder");
    let copy = storage.fetch("syn://syn100", downloads.path()).await;
    let copy = copy.expect("a copy");
    assert_eq!(std::fs::read_to_string(copy).expect("the copy"), CONTENT);
    let requests = service.requests.lock().expect("the requests").clone();
    let lines: Vec<String> = (requests.iter())
        .map(|request| format!("{} {}", request.method, request.path))
        .collect();
    assert!(lines.contains(&"GET /repo/v1/entity/syn100/version/2".into()));
    let presigned = "GET /file/v1/file/9001?redirect=false&fileAssociateType=FileEntity&\
                     fileAssociateId=syn100";
    assert!(lines.contains(&presigned.into()), "{lines:?}");
    for request in &requests {
        let token = request.headers.get("authorization").map(String::as_str);
        let expected = match request.path.starts_with("/bucket/") {
            true => None,
            false => Some("Bearer fake-token"),
        };
        assert_eq!(token, expected, "{}", request.path);
    }
}

#[tokio::test]
async fn publishes_new_versions() {
    let service = synapse();
    let storage = storage(&service);
    let folder = tempfile::tempdir().expect("a folder");
    let file = folder.path().join("out.txt");
    std::fs::write(&file, "published\n").expect("a file");
    let target = "syn://syn200/sub/deeper/out.txt";
    let receipt = storage.upload_with_receipt(&file, target).await;
    assert_eq!(receipt.expect("an upload").version.as_deref(), Some("9100"));
    let first = service.lines();
    assert_eq!(
        first[1..4],
        [
            "POST /repo/v1/entity/children",
            "POST /repo/v1/entity",
            "POST /repo/v1/entity/children",
        ]
    );
    assert!(first.contains(&"PUT /bucket-put/upload-1/1".into()));
    let add = "PUT /file/v1/file/multipart/upload-1/add/1?partMD5Hex=";
    assert!(first.iter().any(|line| line.starts_with(add)), "{first:?}");
    storage
        .upload(&file, target)
        .await
        .expect("a second upload");
    let requests = service.requests.lock().expect("the requests").clone();
    let update = (requests.iter())
        .find(|request| request.method == "PUT" && request.path == "/repo/v1/entity/syn303")
        .expect("a new version of the file entity");
    let body: Value = serde_json::from_slice(&update.body).expect("JSON");
    assert_eq!(
        (&body["etag"], &body["parentId"], &body["dataFileHandleId"]),
        (&json!("etag-syn303"), &json!("syn302"), &json!("9100"))
    );
    let part = (requests.iter())
        .find(|request| request.path.starts_with("/bucket-put/"))
        .expect("a part");
    assert_eq!(part.body, b"published\n");
    let signed = part.headers.get("content-type").map(String::as_str);
    assert_eq!(signed, Some("application/octet-stream"));
    let made = (requests.iter()).filter(|request| request.path == "/repo/v1/entity");
    assert_eq!(made.count(), 3, "two folders and one file entity");
    let error = storage
        .upload(&file, "syn://syn100")
        .await
        .expect_err("no name");
    assert!(
        error.to_string().contains("path must include a filename"),
        "{error}"
    );
}
