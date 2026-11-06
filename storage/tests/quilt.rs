//! The Quilt provider against a local fake of S3: files written into a package, the push of
//! its manifest and pointers, and reads of package versions, also of the recorded manifest.
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
use storage::quilt_package::{Push, push};

type Objects = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// A path-style S3 fake that keeps objects by `/<bucket>/<key>` and answers HEAD, ranged GET
/// and PUT; it does not check signatures.
fn s3(objects: Objects) -> Service {
    Service::start(move |request: &Request, _| {
        let path = request
            .path
            .split('?')
            .next()
            .unwrap_or_default()
            .to_owned();
        let path = path.replace("%2F", "/");
        let mut objects = objects.lock().expect("the objects");
        match request.method.as_str() {
            "PUT" => {
                objects.insert(path, request.body.clone());
                Reply {
                    status: 200,
                    headers: vec![("ETag".into(), "\"put\"".into())],
                    body: Vec::new(),
                }
            }
            "HEAD" | "GET" => match objects.get(&path) {
                Some(body) => {
                    let start = (request.headers.get("range"))
                        .and_then(|range| range.strip_prefix("bytes="))
                        .and_then(|range| range.split('-').next()?.parse::<usize>().ok())
                        .unwrap_or(0);
                    let tag = format!(
                        "\"{}-{}\"",
                        body.len(),
                        body.iter().map(|b| *b as u64).sum::<u64>()
                    );
                    Reply {
                        status: if start > 0 { 206 } else { 200 },
                        headers: vec![("ETag".into(), tag)],
                        body: body[start.min(body.len())..].to_vec(),
                    }
                }
                None => Reply {
                    status: 404,
                    headers: vec![("Content-Type".into(), "application/xml".into())],
                    body: b"<Error><Code>NoSuchKey</Code></Error>".to_vec(),
                },
            },
            _ => Reply::json(405, json!({})),
        }
    })
}

fn storage(service: &Service) -> Storage {
    let text = format!(
        "[storage.quilt]\ntype = \"quilt\"\nprefixes = [\"quilt+s3://\"]\nendpoint = \"{}\"\n\
         region = \"us-east-1\"\naccess_key_id = \"test\"\nsecret_access_key = \"test-secret\"\n",
        service.address
    );
    let config = ConfigFile::parse(&text, Path::new("/home/me/.config/uwe/config.toml"));
    Storage::from_config(&config.expect("TOML")).expect("valid settings")
}

fn text(objects: &Objects, key: &str) -> String {
    let objects = objects.lock().expect("the objects");
    String::from_utf8_lossy(objects.get(key).expect("an object")).into_owned()
}

#[tokio::test]
async fn pushes_written_package() {
    let objects = Objects::default();
    let service = s3(objects.clone());
    let storage = storage(&service);
    let folder = tempfile::tempdir().expect("a folder");
    for (name, content) in [("a.txt", "hello\n"), ("table.tsv", "x\ty\n1\t2\n")] {
        let file = folder.path().join(name);
        std::fs::write(&file, content).expect("a file");
        let target = format!("quilt+s3://pushtest#package=test/written&path=out/{name}");
        storage.upload(&file, &target).await.expect("an upload");
    }
    let mut meta = serde_json::Map::new();
    meta.insert("pipeline".into(), json!("probe"));
    let address = "quilt+s3://pushtest?team=lab#package=test/written".to_owned();
    let summarize = ["*.md", "*.?sv", "**/*.tsv"].map(str::to_owned).to_vec();
    let hash = tokio::task::spawn_blocking(move || {
        push(Push {
            address,
            meta,
            message: "nf-quilt:2026-11-06-\nprobe\n".into(),
            files: vec![("README_NF_QUILT.md".into(), b"# test/written\n".to_vec())],
            summarize: Some(summarize),
        })
    })
    .await
    .expect("the push")
    .expect("a top hash");
    assert_eq!(
        text(
            &objects,
            "/pushtest/.quilt/named_packages/test/written/latest"
        ),
        hash
    );
    let manifest = text(&objects, &format!("/pushtest/.quilt/packages/{hash}"));
    let lines: Vec<Value> = (manifest.lines())
        .map(|line| serde_json::from_str(line).expect("JSON"))
        .collect();
    assert_eq!(
        lines[0]["user_meta"],
        json!({ "pipeline": "probe", "team": "lab" })
    );
    let keys: Vec<&str> = lines[1..]
        .iter()
        .filter_map(|line| line["logical_key"].as_str())
        .collect();
    assert_eq!(
        keys,
        [
            "README_NF_QUILT.md",
            "out/a.txt",
            "out/table.tsv",
            "quilt_summarize.json"
        ]
    );
    assert_eq!(
        lines[2]["physical_keys"],
        json!(["s3://pushtest/test/written/out/a.txt"])
    );
    let sha = "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03";
    assert_eq!(
        (&lines[2]["size"], &lines[2]["hash"]["value"]),
        (&json!(6), &json!(sha))
    );
    let summary: Value = serde_json::from_str(&text(
        &objects,
        "/pushtest/test/written/quilt_summarize.json",
    ))
    .expect("JSON");
    let paths: Vec<&str> = (summary.as_array().into_iter().flatten())
        .filter_map(|item| item["path"].as_str())
        .collect();
    assert_eq!(paths, ["README_NF_QUILT.md", "out/table.tsv"]);
    let puts = service
        .lines()
        .into_iter()
        .filter(|line| line.starts_with("PUT "))
        .count();
    assert_eq!(
        puts, 7,
        "two files, README, summary, manifest and two pointers"
    );
    let requests = service.requests.lock().expect("the requests").clone();
    assert!(
        requests
            .iter()
            .all(|request| request.headers.contains_key("authorization"))
    );
    let pinned = format!("quilt+s3://pushtest#package=test/written@{hash}&path=out/a.txt");
    let found = storage
        .stat(&pinned)
        .await
        .expect("a stat")
        .expect("an entry");
    assert_eq!((found.size, found.version.as_deref()), (6, Some(sha)));
}

#[tokio::test]
async fn reads_recorded_manifest() {
    let objects = Objects::default();
    let manifest = include_bytes!("fixtures/quilt_manifest.jsonl").to_vec();
    let hash = "f5f90e5138ac603e27881b94969cf05916c6ec388aa9946d62365796aa9ca327";
    {
        let mut objects = objects.lock().expect("the objects");
        objects.insert(format!("/nfquilt-test/.quilt/packages/{hash}"), manifest);
        objects.insert(
            "/nfquilt-test/.quilt/named_packages/test/probe/latest".into(),
            hash.as_bytes().to_vec(),
        );
        objects.insert(
            "/nfquilt-test/test/probe/out/a.txt".into(),
            b"hello\n".to_vec(),
        );
    }
    let service = s3(objects);
    let storage = storage(&service);
    let file = "quilt+s3://nfquilt-test#package=test/probe&path=out/a.txt";
    let found = storage.stat(file).await.expect("a stat").expect("an entry");
    assert_eq!((found.directory, found.size), (false, 6));
    let package = storage
        .stat("quilt+s3://nfquilt-test#package=test/probe")
        .await;
    assert!(package.expect("a stat").expect("the package").directory);
    let missing = storage
        .stat("quilt+s3://nfquilt-test#package=test/probe&path=none")
        .await;
    assert_eq!(missing.expect("a stat"), None);
    let mut names: Vec<(String, bool)> = (storage
        .list("quilt+s3://nfquilt-test#package=test/probe")
        .await)
        .expect("a listing")
        .into_iter()
        .map(|entry| (entry.name, entry.directory))
        .collect();
    names.sort();
    let expected = [
        ("README_NF_QUILT.md", false),
        ("nf-quilt", true),
        ("out", true),
        ("quilt_summarize.json", false),
    ]
    .map(|(name, directory)| (name.to_owned(), directory));
    assert_eq!(names, expected);
    let downloads = tempfile::tempdir().expect("a folder");
    let copy = storage.fetch(file, downloads.path()).await.expect("a copy");
    assert_eq!(std::fs::read_to_string(copy).expect("text"), "hello\n");
    let pinned = format!("quilt+s3://nfquilt-test#package=test/probe@{hash}&path=out/table.tsv");
    let table = storage
        .stat(&pinned)
        .await
        .expect("a stat")
        .expect("an entry");
    assert_eq!(table.size, 8);
}

/// An environment variable that `just storage-test` sets.
fn variable(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is set by `just storage-test`"))
}

#[tokio::test]
#[ignore = "needs an S3 server: just storage-test"]
async fn pushes_rustfs_package() {
    let text = format!(
        "[storage.quilt]\ntype = \"quilt\"\nprefixes = [\"quilt+s3://inputs\"]\nendpoint = \"{}\"\n\
         access_key_id = {{ environment = \"UWE_TEST_INPUTS_KEY\" }}\n\
         secret_access_key = {{ environment = \"UWE_TEST_INPUTS_SECRET\" }}\n",
        variable("UWE_TEST_INPUTS_ENDPOINT")
    );
    let config = ConfigFile::parse(&text, Path::new("/test/config.toml")).expect("TOML");
    let storage = Storage::from_config(&config).expect("valid settings");
    let folder = tempfile::tempdir().expect("a folder");
    let file = folder.path().join("a.txt");
    std::fs::write(&file, "hello\n").expect("a file");
    let target = "quilt+s3://inputs#package=test/rustfs&path=out/a.txt";
    storage.upload(&file, target).await.expect("an upload");
    let hash = tokio::task::spawn_blocking(|| {
        push(Push {
            address: "quilt+s3://inputs#package=test/rustfs".into(),
            message: "probe".into(),
            ..Push::default()
        })
    })
    .await
    .expect("the push")
    .expect("a top hash");
    let pinned = format!("quilt+s3://inputs#package=test/rustfs@{hash}&path=out/a.txt");
    let found = storage
        .stat(&pinned)
        .await
        .expect("a stat")
        .expect("an entry");
    assert_eq!(found.size, 6);
    let downloads = folder.path().join("downloads");
    let copy = storage.fetch(target, &downloads).await.expect("a copy");
    assert_eq!(std::fs::read_to_string(copy).expect("text"), "hello\n");
}
