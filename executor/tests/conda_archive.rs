//! Verified image archive transport through a fake Docker-compatible engine.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

use conda_environment::{BuiltImage, CompletedImage, FORMAT, ImagePreparation, Lock};
use content_store::{Store, layout};
use executor::conda::{TransferLimits, restore, save};
use executor::{CancellationToken, ContainerEngine};
use serde_json::json;
use task_model::Digest;

const ARCHIVE: &str = "verified archive bytes";
type Reply = (&'static str, u16, String);

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
        &format!("mamba@sha256:{}", "a".repeat(64)),
        "linux/amd64",
        &[],
    )
    .unwrap()
}

fn receipt(preparation: &ImagePreparation) -> CompletedImage {
    preparation
        .completed(BuiltImage {
            preparation: preparation.digest().into(),
            base: preparation.base().into(),
            recipe_digest: preparation.recipe_digest(),
            platform: preparation.platform().into(),
            image_id: format!("sha256:{}", "b".repeat(64)),
            archive_digest: Some(Digest::sha256(ARCHIVE.as_bytes()).to_string()),
            manifest_digest: None,
            reference: None,
        })
        .unwrap()
}

fn inspect(preparation: &ImagePreparation) -> Reply {
    (
        "/json",
        200,
        json!({
            "Id": receipt(preparation).image_id(), "Os": "linux", "Architecture": "amd64",
            "Config": {"Labels": {
                "org.uwe.conda.preparation": preparation.digest(),
                "org.uwe.conda.base": preparation.base(),
                "org.uwe.conda.recipe": preparation.recipe_digest()
            }}
        })
        .to_string(),
    )
}

fn limits() -> TransferLimits {
    TransferLimits {
        archive_bytes: 1024,
        progress_bytes: 4096,
        timeout: Duration::from_secs(5),
    }
}

async fn engine(
    folder: &std::path::Path,
    replies: Vec<Reply>,
) -> (ContainerEngine, std::thread::JoinHandle<()>) {
    let path = folder.join("engine.sock");
    let listener = UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        for (expected, status, body) in
            [("/version", 200, "{}".into()), ("/info", 200, "{}".into())]
                .into_iter()
                .chain(replies)
        {
            let deadline = Instant::now() + Duration::from_secs(5);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "no request for {expected}");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept {expected}: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(line.contains(expected), "{line} expected {expected}");
            if expected == "/json" || expected == "/get" {
                assert!(line.contains(&format!("sha256:{}", "b".repeat(64))));
            }
            let mut length = 0;
            let mut chunked = false;
            loop {
                line.clear();
                assert_ne!(
                    reader.read_line(&mut line).unwrap(),
                    0,
                    "closed headers for {expected}"
                );
                if line == "\r\n" {
                    break;
                }
                let header = line.to_ascii_lowercase();
                if let Some(value) = header.strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
                chunked |= header.contains("transfer-encoding: chunked");
            }
            let mut request = vec![0; length];
            reader.read_exact(&mut request).unwrap();
            if chunked {
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    let count = usize::from_str_radix(line.trim(), 16).unwrap();
                    let mut chunk = vec![0; count + 2];
                    reader.read_exact(&mut chunk).unwrap();
                    request.extend_from_slice(&chunk[..count]);
                    if count == 0 {
                        break;
                    }
                }
            }
            if expected == "/images/load" {
                assert_eq!(request, ARCHIVE.as_bytes());
            }
            if body == "stall" {
                std::thread::sleep(Duration::from_millis(150));
                continue;
            }
            write!(stream, "HTTP/1.1 {status} Response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    (
        ContainerEngine::connect(path.to_str().unwrap())
            .await
            .unwrap(),
        server,
    )
}

fn missing() -> Reply {
    ("/json", 404, "{\"message\":\"missing\"}".into())
}

fn loaded() -> Reply {
    ("/images/load", 200, "{\"stream\":\"loaded\"}\n".into())
}

#[tokio::test]
async fn reuses_verified_archives() {
    let folder = tempfile::tempdir().unwrap();
    let prepared = preparation();
    let store = Store::memory().unwrap();
    let replies = vec![
        inspect(&prepared),
        ("/get", 200, ARCHIVE.into()),
        missing(),
        loaded(),
        inspect(&prepared),
        inspect(&prepared),
    ];
    let (engine, server) = engine(folder.path(), replies).await;
    let cancel = CancellationToken::new();
    let saved = save(
        &engine,
        &prepared,
        &receipt(&prepared),
        &store,
        limits(),
        &cancel,
    )
    .await
    .unwrap();
    assert_eq!(
        saved.archive_digest(),
        Some(Digest::sha256(ARCHIVE.as_bytes()).to_string().as_str())
    );
    assert_ne!(saved.archive_digest(), Some(saved.image_id()));
    assert!(saved.manifest_digest().is_none() && saved.reference().is_none());
    assert_eq!(
        restore(&engine, &prepared, &saved, &store, limits(), &cancel)
            .await
            .unwrap(),
        saved
    );
    let empty = Store::memory().unwrap();
    assert_eq!(
        restore(&engine, &prepared, &saved, &empty, limits(), &cancel)
            .await
            .unwrap(),
        saved
    );
    server.join().unwrap();
}

#[tokio::test]
async fn rejects_invalid_archives() {
    for corrupt in [true, false] {
        let folder = tempfile::tempdir().unwrap();
        let store = Store::local(&folder.path().join("store")).unwrap();
        let digest = store.put_content(ARCHIVE.as_bytes().into()).await.unwrap();
        let mut bounded = limits();
        if corrupt {
            // Store files are read-only; the test unlocks one to damage it.
            let stored = store.local_path(&layout::content(&digest)).unwrap();
            let unlocked = std::os::unix::fs::PermissionsExt::from_mode(0o644);
            std::fs::set_permissions(&stored, unlocked).unwrap();
            std::fs::write(stored, "corrupted archive data").unwrap();
        } else {
            bounded.archive_bytes = 1;
        }
        let (engine, server) = engine(folder.path(), vec![missing()]).await;
        let prepared = preparation();
        let error = restore(
            &engine,
            &prepared,
            &receipt(&prepared),
            &store,
            bounded,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        if corrupt {
            assert!(matches!(
                error,
                executor::conda::CondaImageError::Store(content_store::StoreError::Corrupt { .. })
            ));
        }
        server.join().unwrap();
    }
}

#[tokio::test]
async fn verifies_loaded_provenance() {
    for fault in ["engine", "labels", "platform", "id", "load"] {
        let folder = tempfile::tempdir().unwrap();
        let prepared = preparation();
        let store = Store::memory().unwrap();
        store.put_content(ARCHIVE.as_bytes().into()).await.unwrap();
        let replies = if fault == "engine" {
            vec![("/json", 500, "{\"message\":\"unavailable\"}".into())]
        } else if fault == "load" {
            vec![
                missing(),
                (
                    "/images/load",
                    200,
                    "{\"errorDetail\":{\"message\":\"bad archive\"}}\n".into(),
                ),
            ]
        } else {
            let mut image: serde_json::Value = serde_json::from_str(&inspect(&prepared).2).unwrap();
            match fault {
                "labels" => image["Config"]["Labels"] = json!({}),
                "platform" => image["Architecture"] = json!("arm64"),
                _ => image["Id"] = json!(format!("sha256:{}", "c".repeat(64))),
            }
            vec![missing(), loaded(), ("/json", 200, image.to_string())]
        };
        let (engine, server) = engine(folder.path(), replies).await;
        assert!(
            restore(
                &engine,
                &prepared,
                &receipt(&prepared),
                &store,
                limits(),
                &CancellationToken::new()
            )
            .await
            .is_err(),
            "{fault}"
        );
        server.join().unwrap();
    }
}

#[tokio::test]
async fn bounds_archive_transfers() {
    for export in [true, false] {
        let folder = tempfile::tempdir().unwrap();
        let prepared = preparation();
        let store = Store::memory().unwrap();
        let replies = if export {
            vec![inspect(&prepared), ("/get", 200, ARCHIVE.into())]
        } else {
            vec![missing(), loaded()]
        };
        let (engine, server) = engine(folder.path(), replies).await;
        let mut bounded = limits();
        let cancel = CancellationToken::new();
        let completed = receipt(&prepared);
        let result = if export {
            bounded.archive_bytes = 1;
            save(&engine, &prepared, &completed, &store, bounded, &cancel).await
        } else {
            store.put_content(ARCHIVE.as_bytes().into()).await.unwrap();
            bounded.progress_bytes = 1;
            restore(&engine, &prepared, &completed, &store, bounded, &cancel).await
        };
        assert!(matches!(
            result,
            Err(executor::conda::CondaImageError::Limit(_))
        ));
        if export {
            assert!(
                !store
                    .has_content(&Digest::sha256(ARCHIVE.as_bytes()))
                    .await
                    .unwrap()
            );
        }
        server.join().unwrap();
    }
}

#[tokio::test]
async fn bounds_cancelled_restore() {
    for cancelled in [true, false] {
        let folder = tempfile::tempdir().unwrap();
        let replies = if cancelled {
            vec![]
        } else {
            vec![("/json", 200, "stall".into())]
        };
        let (engine, server) = engine(folder.path(), replies).await;
        let prepared = preparation();
        let cancel = CancellationToken::new();
        let mut bounded = limits();
        if cancelled {
            cancel.cancel();
        } else {
            bounded.timeout = Duration::from_millis(50);
        }
        let error = restore(
            &engine,
            &prepared,
            &receipt(&prepared),
            &Store::memory().unwrap(),
            bounded,
            &cancel,
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            executor::conda::CondaImageError::Cancelled
                | executor::conda::CondaImageError::Limit("wall-clock")
        ));
        server.join().unwrap();
    }
}
