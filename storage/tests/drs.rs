//! The DRS provider against a local fake of a DRS server with a Gen3 token exchange: object
//! lookup, access methods, token reuse and renewal, and missing objects.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::service;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use base64::Engine;
use config_file::ConfigFile;
use serde_json::json;
use service::{Reply, Request, Service};
use storage::{Storage, StorageError};

const CONTENT: &str = "ACGT drs object\n";

fn storage(service: &Service, extra: &str) -> Storage {
    let host = service.address.trim_start_matches("http://");
    let text = format!(
        "[storage.commons]\ntype = \"drs\"\nprefixes = [\"drs://\"]\nplain_http = [\"{host}\"]\n\
         {extra}"
    );
    let config = ConfigFile::parse(&text, Path::new("/home/me/.config/uwe/config.toml"));
    Storage::from_config(&config.expect("TOML")).expect("valid settings")
}

fn token(expires: u64) -> String {
    let encode = |text: String| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text);
    let claims = format!("{{\"exp\":{expires}}}");
    format!(
        "{}.{}.sig",
        encode("{\"alg\":\"none\"}".into()),
        encode(claims)
    )
}

/// The fake: object `dg.TEST/abc-123` with an `https` and an `s3` access id, object
/// `obj-inline` with an inline URL, and a token exchange whose answers `exchanges` counts.
fn commons(exchanges: Arc<AtomicUsize>) -> Service {
    Service::start(move |request: &Request, base: &str| {
        let path = request.path.as_str();
        if path == "/user/credentials/cdis/access_token" {
            let count = exchanges.fetch_add(1, Ordering::SeqCst);
            return Reply::json(
                200,
                json!({ "access_token": token(4102444800 + count as u64) }),
            );
        }
        let bearer = request.headers.get("authorization").cloned();
        if path.starts_with("/ga4gh/")
            && !bearer
                .as_ref()
                .is_some_and(|value| value.starts_with("Bearer "))
        {
            return Reply::json(401, json!({ "msg": "no token" }));
        }
        match path {
            "/ga4gh/drs/v1/objects/dg.TEST/abc-123" => Reply::json(
                200,
                json!({
                    "id": "dg.TEST/abc-123",
                    "name": "sample1.bam",
                    "size": CONTENT.len(),
                    "updated_time": "2026-01-03T03:04:05Z",
                    "checksums": [{ "type": "md5", "checksum": "f00" }],
                    "access_methods": [
                        { "type": "https", "access_id": "https-id" },
                        { "type": "s3", "access_id": "s3-id",
                          "access_url": { "url": "s3://bucket/sample1.bam" } },
                    ],
                }),
            ),
            "/ga4gh/drs/v1/objects/obj-inline" => Reply::json(
                200,
                json!({
                    "id": "obj-inline",
                    "size": CONTENT.len(),
                    "access_methods": [{ "type": "https",
                        "access_url": { "url": format!("{base}/bucket/inline.txt") } }],
                }),
            ),
            _ if let Some(id) =
                path.strip_prefix("/ga4gh/drs/v1/objects/dg.TEST/abc-123/access/") =>
            {
                Reply::json(200, json!({ "url": format!("{base}/bucket/{id}") }))
            }
            _ if path.starts_with("/bucket/") && bearer.is_none() => Reply {
                status: 200,
                headers: Vec::new(),
                body: CONTENT.as_bytes().to_vec(),
            },
            _ => Reply::json(404, json!({ "msg": "not found" })),
        }
    })
}

#[tokio::test]
async fn reads_gen3_objects() {
    let exchanges = Arc::new(AtomicUsize::new(0));
    let service = commons(exchanges.clone());
    let host = service.address.trim_start_matches("http://").to_owned();
    let storage = storage(
        &service,
        "credential = \"api-key\"\naccess_protocol = \"https\"\n",
    );
    let object = format!("drs://{host}/dg.TEST/abc-123");
    let found = storage
        .stat(&object)
        .await
        .expect("a stat")
        .expect("an object");
    assert_eq!((found.directory, found.size), (false, CONTENT.len() as u64));
    assert_eq!(found.version.as_deref(), Some("md5:f00"));
    let downloads = tempfile::tempdir().expect("a folder");
    let copy = storage
        .fetch(&object, downloads.path())
        .await
        .expect("a copy");
    assert_eq!(std::fs::read_to_string(copy).expect("the copy"), CONTENT);
    let inline = format!("drs://{host}/obj-inline");
    let copy = storage
        .fetch(&inline, downloads.path())
        .await
        .expect("a copy");
    assert_eq!(std::fs::read_to_string(copy).expect("the copy"), CONTENT);
    let missing = storage.stat(&format!("drs://{host}/no-such-object")).await;
    assert!(missing.expect("a stat").is_none());
    assert_eq!(storage.list(&object).await.expect("a listing"), Vec::new());
    assert_eq!(exchanges.load(Ordering::SeqCst), 1, "one exchange per host");
    let lines = service.lines();
    assert_eq!(lines[0], "POST /user/credentials/cdis/access_token");
    assert!(lines.contains(&"GET /ga4gh/drs/v1/objects/dg.TEST/abc-123/access/https-id".into()));
    let token = (service.requests.lock().expect("the requests").iter())
        .find(|request| request.path.ends_with("/access_token"))
        .map(|request| String::from_utf8_lossy(&request.body).into_owned());
    assert_eq!(token.as_deref(), Some("{\"api_key\":\"api-key\"}"));
}

#[tokio::test]
async fn refuses_without_tokens() {
    let service = commons(Arc::new(AtomicUsize::new(0)));
    let host = service.address.trim_start_matches("http://").to_owned();
    let anonymous = storage(&service, "authentication = \"none\"\n");
    let object = format!("drs://{host}/dg.TEST/abc-123");
    let error = anonymous.stat(&object).await.expect_err("a refusal");
    assert!(
        error
            .to_string()
            .contains("DRS authentication failed (HTTP 401)"),
        "{error}"
    );
    let unknown = storage(&service, "authentication = \"kerberos\"\n");
    let error = unknown.stat(&object).await.expect_err("an unknown login");
    assert!(matches!(error, StorageError::Unsupported(_)), "{error}");
    let bearer = storage(
        &service,
        "authentication = \"bearer\"\ncredential = \"t0k3n\"\n",
    );
    let copy = tempfile::tempdir().expect("a folder");
    let file = copy.path().join("object");
    bearer.download(&object, &file).await.expect("a download");
    assert_eq!(std::fs::read_to_string(file).expect("the copy"), CONTENT);
    let s3 = "GET /ga4gh/drs/v1/objects/dg.TEST/abc-123/access/s3-id".to_owned();
    assert!(service.lines().contains(&s3), "the s3 method comes first");
}

/// Two runs of one process with other `drs` scopes each sign in with their own, also when
/// they read at the same time, and neither reaches storages without a scope.
#[test]
fn keeps_run_scopes() {
    let scoped = |variable: &str| {
        let options = storage::drs::Options {
            authentication: Some("bearer".into()),
            variable: Some(variable.to_owned()),
            ..storage::drs::Options::default()
        };
        let scopes = storage::Scopes {
            drs: Some(options),
            ..storage::Scopes::default()
        };
        Storage::default().with_scopes(scopes)
    };
    let (first, second) = (
        scoped("UWE_TEST_UNSET_FIRST"),
        scoped("UWE_TEST_UNSET_SECOND"),
    );
    let read = |storage: Storage| {
        std::thread::spawn(move || {
            let blocking = storage.session().blocking();
            (0..20)
                .map(|_| {
                    blocking
                        .stat("drs://127.0.0.1:9/object")
                        .expect_err("no token")
                })
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
        })
    };
    let (one, two) = (read(first), read(second));
    for (errors, own, other) in [
        (one.join().unwrap(), "FIRST", "SECOND"),
        (two.join().unwrap(), "SECOND", "FIRST"),
    ] {
        for error in errors {
            assert!(error.contains(&format!("UWE_TEST_UNSET_{own}")), "{error}");
            assert!(!error.contains(other), "{error}");
        }
    }
    let unscoped = Storage::default()
        .blocking()
        .stat("drs://127.0.0.1:9/object");
    let unscoped = format!("{unscoped:?}");
    assert!(!unscoped.contains("UWE_TEST_UNSET"), "{unscoped}");
}

/// The storage of a run with its plugin scopes keeps the pins that a repeat set before.
#[test]
fn keeps_scoped_pins() {
    let folder = tempfile::tempdir().unwrap();
    let copy = folder.path().join("copy");
    std::fs::write(&copy, "first\n").unwrap();
    let storage = Storage::default();
    let address = "https://example.org/sheet.txt";
    storage.pin(storage::Fetched {
        address: address.into(),
        copy: copy.clone(),
        digest: format!("sha256:{}", hex(b"first\n")),
        size: 6,
        time: std::time::SystemTime::now(),
        version: None,
    });
    let scoped = storage.with_scopes(storage::Scopes::default()).blocking();
    let found = scoped
        .fetch(address, &folder.path().join("downloads"))
        .unwrap();
    assert_eq!(std::fs::read_to_string(found).unwrap(), "first\n");
}

fn hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
