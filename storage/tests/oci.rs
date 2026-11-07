//! The OCI provider against a local fake of Object Storage that checks the signature of every
//! request: namespace, stat, list, download and upload.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "common/service.rs"]
mod service;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use aws_lc_rs::encoding::AsDer;
use aws_lc_rs::rsa::KeySize;
use aws_lc_rs::signature::{KeyPair, RSA_PKCS1_2048_8192_SHA256, RsaKeyPair, UnparsedPublicKey};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use config_file::ConfigFile;
use serde_json::json;
use service::{Reply, Request, Service};
use storage::{Entry, Storage};

const KEY_ID: &str = "ocid1.tenancy.test/ocid1.user.test/aa:bb";

/// A fake with the namespace `testns` and the bucket `data`; it answers 401 to a request
/// whose signature does not verify with `public`.
fn object_storage(public: Vec<u8>) -> Service {
    let objects = Arc::new(Mutex::new(HashMap::from([
        ("a/in.txt".to_owned(), b"hello\n".to_vec()),
        ("a/sub/b.csv".to_owned(), b"x,y\n".to_vec()),
    ])));
    Service::start(move |request: &Request, _| {
        if !signed(request, &public) {
            return Reply::json(401, json!({ "code": "NotAuthenticated", "message": "bad" }));
        }
        let (path, query) = request.path.split_once('?').unwrap_or((&request.path, ""));
        let mut objects = objects.lock().expect("the objects");
        let object = path.strip_prefix("/n/testns/b/data/o/").map(decode);
        match (request.method.as_str(), path, object) {
            ("GET", "/n/", _) => Reply::json(200, json!("testns")),
            ("GET", "/n/testns/b/data", _) => Reply::json(200, json!({ "name": "data" })),
            ("GET", "/n/testns/b/data/o", _) => {
                let prefix = (query.split('&'))
                    .find_map(|pair| pair.strip_prefix("prefix="))
                    .map(decode)
                    .unwrap_or_default();
                let mut files = Vec::new();
                let mut folders = Vec::new();
                for name in objects.keys().filter(|name| name.starts_with(&prefix)) {
                    match name[prefix.len()..].split_once('/') {
                        Some((folder, _)) => folders.push(format!("{prefix}{folder}/")),
                        None => files.push(json!({ "name": name })),
                    }
                }
                folders.dedup();
                Reply::json(200, json!({ "objects": files, "prefixes": folders }))
            }
            ("HEAD" | "GET", _, Some(name)) => match objects.get(&name) {
                Some(body) => Reply {
                    status: 200,
                    headers: vec![("ETag".into(), format!("tag-{}", body.len()))],
                    body: body.clone(),
                },
                None => Reply::json(404, json!({ "code": "ObjectNotFound" })),
            },
            ("PUT", _, Some(name)) => {
                objects.insert(name, request.body.clone());
                Reply {
                    status: 200,
                    headers: vec![("ETag".into(), "tag-new".into())],
                    body: Vec::new(),
                }
            }
            _ => Reply::json(404, json!({ "code": "NotFound" })),
        }
    })
}

/// Whether the `Authorization` header signs the date, the request target and the host with
/// the key, as OCI's draft-cavage signatures do.
fn signed(request: &Request, public: &[u8]) -> bool {
    let Some(authorization) = request.headers.get("authorization") else {
        return false;
    };
    let field = |name: &str| {
        let start = authorization.find(&format!("{name}=\""))? + name.len() + 2;
        let end = authorization[start..].find('"')? + start;
        Some(authorization[start..end].to_owned())
    };
    let (Some(names), Some(signature)) = (field("headers"), field("signature")) else {
        return false;
    };
    if field("keyId").as_deref() != Some(KEY_ID) || names != "date (request-target) host" {
        return false;
    }
    let text = format!(
        "date: {}\n(request-target): {} {}\nhost: {}",
        request.headers.get("date").map_or("", String::as_str),
        request.method.to_lowercase(),
        request.path,
        request.headers.get("host").map_or("", String::as_str),
    );
    let signature = STANDARD.decode(signature).unwrap_or_default();
    UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, public)
        .verify(text.as_bytes(), &signature)
        .is_ok()
}

fn decode(text: &str) -> String {
    let mut bytes = Vec::new();
    let mut rest = text.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        match (byte, tail.get(..2)) {
            (b'%', Some(pair)) => {
                let pair = std::str::from_utf8(pair).unwrap_or("00");
                bytes.push(u8::from_str_radix(pair, 16).unwrap_or(0));
                rest = &tail[2..];
            }
            _ => {
                bytes.push(byte);
                rest = tail;
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A new API key as PKCS#8 PEM text, with its public key.
fn api_key() -> (String, Vec<u8>) {
    let pair = RsaKeyPair::generate(KeySize::Rsa2048).expect("a key");
    let der = pair.as_der().expect("PKCS#8");
    let encoded = STANDARD.encode(der.as_ref());
    let lines: Vec<&str> = (encoded.as_bytes().chunks(64))
        .map(|line| std::str::from_utf8(line).expect("base64"))
        .collect();
    let pem = format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        lines.join("\n")
    );
    (pem, pair.public_key().as_ref().to_vec())
}

fn storage(service: &Service, pem: &str) -> Storage {
    let text = format!(
        "[storage.oci]\ntype = \"oci\"\nprefixes = [\"oci://\"]\nendpoint = \"{}\"\n\
         tenancy = \"ocid1.tenancy.test\"\nuser = \"ocid1.user.test\"\nfingerprint = \"aa:bb\"\n\
         private_key = \"\"\"\n{pem}\"\"\"\n",
        service.address
    );
    let config = ConfigFile::parse(&text, Path::new("/home/me/.config/uwe/config.toml"));
    Storage::from_config(&config.expect("TOML")).expect("valid settings")
}

#[tokio::test]
async fn reads_signed_objects() {
    let (pem, public) = api_key();
    let service = object_storage(public);
    let storage = storage(&service, &pem);
    let file = storage.stat("oci:///data/a/in.txt").await.expect("a stat");
    let file = file.expect("the object");
    assert_eq!((file.directory, file.size), (false, 6));
    assert_eq!(file.version.as_deref(), Some("tag-6"));
    let folder = storage.stat("oci://data/a/sub").await.expect("a stat");
    assert!(folder.expect("a folder").directory);
    assert!(
        storage
            .stat("oci:///data/none.txt")
            .await
            .expect("a stat")
            .is_none()
    );
    let mut entries = storage.list("oci:///data/a/").await.expect("a listing");
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    let expected = [("in.txt", false), ("sub", true)].map(|(name, directory)| Entry {
        name: name.into(),
        directory,
    });
    assert_eq!(entries, expected);
    let downloads = tempfile::tempdir().expect("a folder");
    let copy = storage
        .fetch("oci:///data/a/in.txt", downloads.path())
        .await;
    assert_eq!(
        std::fs::read_to_string(copy.expect("a copy")).expect("text"),
        "hello\n"
    );
    let lines = service.lines();
    assert_eq!(lines.iter().filter(|line| *line == "GET /n/").count(), 1);
    assert!(
        lines.contains(&"HEAD /n/testns/b/data/o/a%2Fin.txt".to_owned()),
        "{lines:?}"
    );
}

#[tokio::test]
async fn writes_signed_objects() {
    let (pem, public) = api_key();
    let service = object_storage(public);
    let storage = storage(&service, &pem);
    let folder = tempfile::tempdir().expect("a folder");
    let file = folder.path().join("out.txt");
    std::fs::write(&file, "published\n").expect("a file");
    let receipt = storage
        .upload_with_receipt(&file, "oci:///data/results/out.txt")
        .await;
    let receipt = receipt.expect("an upload");
    assert_eq!(
        (receipt.size, receipt.version.as_deref()),
        (10, Some("tag-new"))
    );
    let requests = service.requests.lock().expect("the requests").clone();
    let put = (requests.iter())
        .find(|request| request.method == "PUT")
        .expect("a PUT");
    assert_eq!(put.path, "/n/testns/b/data/o/results%2Fout.txt");
    assert_eq!(put.body, b"published\n");
    let (other, _) = api_key();
    let refused = storage_error(&service, &other).await;
    assert!(refused.contains("401"), "{refused}");
    assert!(!refused.contains("PRIVATE KEY"), "{refused}");
}

async fn storage_error(service: &Service, pem: &str) -> String {
    let storage = storage(service, pem);
    let error = storage
        .stat("oci:///data/a/in.txt")
        .await
        .expect_err("a wrong key");
    error.to_string()
}
