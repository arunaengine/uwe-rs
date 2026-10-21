//! Docker API fixtures qualify the adapter contract without installing anything on the host.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::time::Duration;

use conda_environment::{FORMAT, ImagePreparation, Lock};
use executor::conda::{BuildLimits, CondaImageError, build, require_base};
use executor::{CancellationToken, ContainerEngine};
use serde_json::{Value, json};

fn preparation() -> ImagePreparation {
    let lock = Lock {
        format: FORMAT,
        platform: "linux-64".into(),
        channels: vec![],
        specs: vec![],
        pip: vec![],
        pypi: None,
        packages: vec![],
        virtual_packages: Some(vec![]),
    };
    ImagePreparation::new(
        &lock,
        &format!("micromamba@sha256:{}", "a".repeat(64)),
        "linux/amd64",
        &[],
    )
    .unwrap()
}

fn limits() -> BuildLimits {
    BuildLimits {
        context_bytes: 1 << 20,
        progress_bytes: 1 << 20,
        timeout: Duration::from_secs(5),
        memory_bytes: 128 << 20,
        cpu_quota: 100_000,
    }
}

fn inspected(preparation: &ImagePreparation) -> Value {
    json!({
        "Id": format!("sha256:{}", "b".repeat(64)), "Os": "linux", "Architecture": "amd64",
        "RepoDigests": [format!("registry/image@sha256:{}", "c".repeat(64))],
        "Config": {"Labels": {
            "org.iunctus.conda.preparation": preparation.digest(),
            "org.iunctus.conda.base": preparation.base(),
            "org.iunctus.conda.recipe": preparation.recipe_digest()
        }}
    })
}

async fn fixture(
    folder: &std::path::Path,
    responses: Vec<(String, String)>,
) -> (ContainerEngine, std::thread::JoinHandle<Vec<String>>) {
    let socket = folder.join("engine.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || {
        let mut paths = Vec::new();
        let replies = [
            vec![
                ("/version".into(), "{}".into()),
                ("/info".into(), "{}".into()),
            ],
            responses,
        ]
        .concat();
        for (expected, body) in replies {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let path = line.split_whitespace().nth(1).unwrap().to_owned();
            assert!(path.contains(&expected), "{path} expected {expected}");
            paths.push(path);
            let mut length = 0;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut request = vec![0; length];
            reader.read_exact(&mut request).unwrap();
            if expected == "/build?" {
                assert!(request.windows(10).any(|bytes| bytes == b"Dockerfile"));
            }
            if body == "stall" {
                std::thread::sleep(Duration::from_millis(150));
                continue;
            }
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
        paths
    });
    let engine = ContainerEngine::connect(socket.to_str().unwrap())
        .await
        .unwrap();
    (engine, server)
}

fn success() -> (String, String) {
    (
        "/build?".into(),
        format!("{{\"aux\":{{\"ID\":\"sha256:{}\"}}}}\n", "b".repeat(64)),
    )
}

#[tokio::test]
async fn requires_base_identity() {
    for changed in [
        "none",
        "digest",
        "missing digest",
        "os",
        "architecture",
        "variant",
    ] {
        let folder = tempfile::tempdir().unwrap();
        let preparation = preparation();
        let mut image = inspected(&preparation);
        image["RepoDigests"] = json!([format!(
            "docker.io/library/micromamba@sha256:{}",
            "a".repeat(64)
        )]);
        match changed {
            "digest" => {
                image["RepoDigests"] = json!([format!("micromamba@sha256:{}", "c".repeat(64))])
            }
            "missing digest" => image["RepoDigests"] = json!(null),
            "os" => image["Os"] = json!("windows"),
            "architecture" => image["Architecture"] = json!("arm64"),
            "variant" => image["Variant"] = json!("v8"),
            _ => {}
        }
        let (engine, server) =
            fixture(folder.path(), vec![("/json".into(), image.to_string())]).await;
        let result = require_base(&engine, &preparation).await;
        if changed == "none" {
            result.unwrap();
        } else {
            assert!(
                matches!(result, Err(CondaImageError::Provenance(_))),
                "{changed}"
            );
        }
        let paths = server.join().unwrap();
        assert_eq!(paths.len(), 3);
        assert!(paths[2].contains("micromamba") && paths[2].contains(&"a".repeat(64)));
    }
}

#[tokio::test]
async fn verifies_bounded_builds() {
    let folder = tempfile::tempdir().unwrap();
    let preparation = preparation();
    let replies = vec![
        success(),
        ("/json".into(), inspected(&preparation).to_string()),
    ];
    let (engine, server) = fixture(folder.path(), replies).await;
    let receipt = build(
        &engine,
        &preparation,
        folder.path(),
        limits(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(receipt.image_id(), format!("sha256:{}", "b".repeat(64)));
    assert!(receipt.manifest_digest().is_none());
    assert!(receipt.archive_digest().is_none());
    let paths = server.join().unwrap();
    for required in [
        "version=1",
        "networkmode=none",
        "memory=134217728",
        "memswap=134217728",
        "cpuquota=100000",
        "platform=linux%2Famd64",
        "org.iunctus.conda.preparation",
        "org.iunctus.conda.base",
        "org.iunctus.conda.recipe",
    ] {
        assert!(paths[2].contains(required), "{required}: {}", paths[2]);
    }
    assert!(paths[3].contains(&format!("sha256:{}", "b".repeat(64))));
}

#[tokio::test]
async fn rejects_wrong_provenance() {
    for field in ["labels", "platform", "id"] {
        let folder = tempfile::tempdir().unwrap();
        let preparation = preparation();
        let mut image = inspected(&preparation);
        match field {
            "labels" => image["Config"]["Labels"] = json!({}),
            "platform" => image["Architecture"] = json!("arm64"),
            _ => image["Id"] = json!(format!("sha256:{}", "d".repeat(64))),
        }
        let (engine, server) = fixture(
            folder.path(),
            vec![success(), ("/json".into(), image.to_string())],
        )
        .await;
        assert!(
            build(
                &engine,
                &preparation,
                folder.path(),
                limits(),
                &CancellationToken::new()
            )
            .await
            .is_err(),
            "{field}"
        );
        server.join().unwrap();
    }
}

#[tokio::test]
async fn rejects_unidentified_builds() {
    for response in [
        "{\"errorDetail\":{\"message\":\"installation failed\"}}\n",
        "{\"stream\":\"Successfully built deadbeef\\n\"}\n",
    ] {
        let folder = tempfile::tempdir().unwrap();
        let (engine, server) =
            fixture(folder.path(), vec![("/build?".into(), response.into())]).await;
        assert!(
            build(
                &engine,
                &preparation(),
                folder.path(),
                limits(),
                &CancellationToken::new()
            )
            .await
            .is_err()
        );
        assert_eq!(server.join().unwrap().len(), 3);
    }
}

#[tokio::test]
async fn bounds_build_submission() {
    for cancelled in [true, false] {
        let folder = tempfile::tempdir().unwrap();
        let (engine, server) = fixture(folder.path(), vec![]).await;
        let cancel = CancellationToken::new();
        let mut bounded = limits();
        if cancelled {
            cancel.cancel();
        } else {
            bounded.context_bytes = 16;
        }
        let error = build(&engine, &preparation(), folder.path(), bounded, &cancel)
            .await
            .unwrap_err();
        if cancelled {
            assert!(matches!(error, CondaImageError::Cancelled));
        } else {
            assert!(error.to_string().contains("byte limit"));
        }
        assert_eq!(server.join().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn cancels_submitted_builds() {
    for cancelled in [true, false] {
        let folder = tempfile::tempdir().unwrap();
        let (engine, server) =
            fixture(folder.path(), vec![("/build?".into(), "stall".into())]).await;
        let cancel = CancellationToken::new();
        let mut bounded = limits();
        if cancelled {
            let trigger = cancel.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                trigger.cancel();
            });
        } else {
            bounded.timeout = Duration::from_millis(50);
        }
        let error = build(&engine, &preparation(), folder.path(), bounded, &cancel)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            CondaImageError::Cancelled | CondaImageError::Limit("wall-clock")
        ));
        assert_eq!(server.join().unwrap().len(), 3);
    }
}
