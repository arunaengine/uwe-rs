//! Cache ownership, verified local reuse, and publication through a bounded fake engine.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

use conda_environment::{
    BuiltImage, CompletedImage, FORMAT, IMAGE_CACHE_NAMESPACE, ImageBuild, ImageCache,
    ImagePreparation, Lock,
};
use content_store::{Holder, Store, layout};
use executor::conda::{BuildLimits, CondaImageError, TransferLimits};
use executor::conda_cache::{CacheLimits, ensure, ensure_with};
use executor::{CancellationToken, ContainerEngine};
use serde_json::json;
use task_model::Digest;

const ARCHIVE: &str = "verified archive bytes";
type Reply = (String, u16, String);

fn preparation() -> ImagePreparation {
    let lock = Lock {
        format: FORMAT,
        platform: "linux-64".into(),
        channels: vec![],
        specs: vec![],
        pip: vec![],
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

fn receipt(p: &ImagePreparation) -> CompletedImage {
    p.completed(BuiltImage {
        preparation: p.digest().into(),
        base: p.base().into(),
        recipe_digest: p.recipe_digest(),
        platform: p.platform().into(),
        image_id: format!("sha256:{}", "b".repeat(64)),
        archive_digest: Some(Digest::sha256(ARCHIVE.as_bytes()).to_string()),
        manifest_digest: None,
        reference: None,
    })
    .unwrap()
}

fn inspect(p: &ImagePreparation) -> Reply {
    (format!("sha256:{}/json", "b".repeat(64)), 200, json!({
        "Id": receipt(p).image_id(), "Os": "linux", "Architecture": "amd64",
        "Config": {"Labels": {"org.iunctus.conda.preparation": p.digest(), "org.iunctus.conda.base": p.base(), "org.iunctus.conda.recipe": p.recipe_digest()}}
    }).to_string())
}

fn images(ids: &[char]) -> Reply {
    let images: Vec<_> = ids.iter().map(|id| json!({
        "Id": format!("sha256:{}", id.to_string().repeat(64)), "ParentId": "", "RepoTags": [], "RepoDigests": [],
        "Created": 0, "Size": 0, "SharedSize": 0, "Labels": {}, "Containers": 0
    })).collect();
    ("/images/json?".into(), 200, json!(images).to_string())
}

fn export() -> Reply {
    ("/get".into(), 200, ARCHIVE.into())
}

fn limits() -> CacheLimits {
    CacheLimits {
        build: BuildLimits {
            context_bytes: 8192,
            progress_bytes: 4096,
            timeout: Duration::from_secs(2),
            memory_bytes: 128 << 20,
            cpu_quota: 100_000,
        },
        transfer: TransferLimits {
            archive_bytes: 1024,
            progress_bytes: 4096,
            timeout: Duration::from_secs(2),
        },
        timeout: Duration::from_secs(5),
        attempts: 2,
        retry_delay: Duration::from_millis(1),
        candidates: 4,
    }
}

async fn engine(
    path: &std::path::Path,
    replies: Vec<Reply>,
) -> (ContainerEngine, std::thread::JoinHandle<()>) {
    let socket = path.join("engine.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        for (expected, status, body) in [
            ("/version".into(), 200, "{}".into()),
            ("/info".into(), 200, "{}".into()),
        ]
        .into_iter()
        .chain(replies)
        {
            let deadline = Instant::now() + Duration::from_secs(5);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "no request for {expected}");
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(line.contains(&expected), "{line} expected {expected}");
            if expected == "/images/json?" {
                assert!(line.contains("org.iunctus.conda.preparation"));
            }
            let mut size = 0;
            let mut chunked = false;
            loop {
                line.clear();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    size = value.trim().parse().unwrap();
                }
                chunked |= line
                    .to_ascii_lowercase()
                    .contains("transfer-encoding: chunked");
            }
            reader.read_exact(&mut vec![0; size]).unwrap();
            if chunked {
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    let size = usize::from_str_radix(line.trim(), 16).unwrap();
                    reader.read_exact(&mut vec![0; size]).unwrap();
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if size == 0 {
                        break;
                    }
                }
            }
            write!(stream, "HTTP/1.1 {status} Response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    (
        ContainerEngine::connect(socket.to_str().unwrap())
            .await
            .unwrap(),
        server,
    )
}

#[tokio::test]
async fn deterministic_local_reuse_publishes_and_next_workflow_skips_build() {
    let folder = tempfile::tempdir().unwrap();
    let p = preparation();
    let bad = (format!("sha256:{}/json", "a".repeat(64)), 200, "{}".into());
    let replies = vec![
        images(&['b', 'a']),
        bad,
        inspect(&p),
        inspect(&p),
        export(),
        inspect(&p),
        inspect(&p),
    ];
    let (engine, server) = engine(folder.path(), replies).await;
    let store = Store::local(&folder.path().join("cache")).unwrap();
    let cancel = CancellationToken::new();
    for run in ["first", "second"] {
        let image = ensure_with(
            &engine,
            &p,
            folder.path(),
            &store,
            &Holder::new(run, "test"),
            limits(),
            &cancel,
            || async {
                Err(CondaImageError::Provenance(
                    "hot reuse called preparation".into(),
                ))
            },
        )
        .await
        .unwrap();
        assert_eq!(image, receipt(&p));
    }
    server.join().unwrap();
}

#[tokio::test]
async fn cold_cache_builds_once_and_publishes_verified_archive() {
    let folder = tempfile::tempdir().unwrap();
    let p = preparation();
    let build = (
        "/build?".into(),
        200,
        format!("{{\"aux\":{{\"ID\":\"{}\"}}}}\n", receipt(&p).image_id()),
    );
    let (engine, server) = engine(
        folder.path(),
        vec![
            images(&[]),
            build,
            inspect(&p),
            inspect(&p),
            export(),
            inspect(&p),
        ],
    )
    .await;
    let store = Store::local(&folder.path().join("cache")).unwrap();
    let calls = std::cell::Cell::new(0);
    let image = ensure_with(
        &engine,
        &p,
        folder.path(),
        &store,
        &Holder::new("cold", "test"),
        limits(),
        &CancellationToken::new(),
        || {
            calls.set(calls.get() + 1);
            async { Ok(()) }
        },
    )
    .await
    .unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(
        ImageCache::new(store)
            .lookup(&p)
            .await
            .unwrap()
            .unwrap()
            .receipt(),
        &image
    );
    server.join().unwrap();
}

#[tokio::test]
async fn a_shared_archive_restores_without_preparing_prerequisites() {
    let folder = tempfile::tempdir().unwrap();
    let p = preparation();
    let store = Store::local(&folder.path().join("cache")).unwrap();
    store.put_content(ARCHIVE.as_bytes().into()).await.unwrap();
    store
        .put_entry(
            IMAGE_CACHE_NAMESPACE,
            &p.digest().parse().unwrap(),
            serde_json::to_vec(&receipt(&p)).unwrap().into(),
        )
        .await
        .unwrap();
    let (engine, server) = engine(
        folder.path(),
        vec![
            (
                "/json".into(),
                404,
                "{\"message\":\"missing local image\"}".into(),
            ),
            (
                "/images/load".into(),
                200,
                "{\"stream\":\"loaded\"}\n".into(),
            ),
            inspect(&p),
        ],
    )
    .await;
    let image = ensure_with(
        &engine,
        &p,
        &folder.path().join("absent packages"),
        &store,
        &Holder::new("restore", "test"),
        limits(),
        &CancellationToken::new(),
        || async {
            Err(CondaImageError::Provenance(
                "archive reuse called preparation".into(),
            ))
        },
    )
    .await
    .unwrap();
    assert_eq!(image, receipt(&p));
    server.join().unwrap();
}

#[tokio::test]
async fn cold_missing_denied_broken_or_wrong_platform_base_stops_before_build() {
    for status in [404, 403, 500, 200] {
        let folder = tempfile::tempdir().unwrap();
        let p = preparation();
        let store = Store::local(&folder.path().join("cache")).unwrap();
        let body = if status == 200 {
            json!({"RepoDigests": [p.base()], "Os": "linux", "Architecture": "arm64"}).to_string()
        } else {
            json!({"message": "base is unavailable"}).to_string()
        };
        let (engine, server) = engine(
            folder.path(),
            vec![images(&[]), ("/json".into(), status, body)],
        )
        .await;
        let result = ensure_with(
            &engine,
            &p,
            folder.path(),
            &store,
            &Holder::new("cold", "test"),
            limits(),
            &CancellationToken::new(),
            || executor::conda::require_base(&engine, &p),
        )
        .await;
        if status == 200 {
            assert!(matches!(result, Err(CondaImageError::Provenance(_))));
        } else {
            assert!(matches!(result, Err(CondaImageError::Engine(
                bollard::errors::Error::DockerResponseServerError { status_code, .. }
            )) if status_code == status));
        }
        assert!(ImageCache::new(store).lookup(&p).await.unwrap().is_none());
        server.join().unwrap();
    }
}

#[tokio::test]
async fn cold_preparation_obeys_total_timeout_cancellation_and_lease_ownership() {
    for stop in ["timeout", "cancel", "lease"] {
        let folder = tempfile::tempdir().unwrap();
        let p = preparation();
        let store = Store::local(&folder.path().join("cache")).unwrap();
        let (engine, server) = engine(folder.path(), vec![images(&[])]).await;
        let cancel = CancellationToken::new();
        let called = std::cell::Cell::new(false);
        let mut bounded = limits();
        bounded.timeout = Duration::from_millis(250);
        let result = ensure_with(
            &engine,
            &p,
            folder.path(),
            &store,
            &Holder::new("cold", "test"),
            bounded,
            &cancel,
            || {
                called.set(true);
                if stop == "cancel" {
                    cancel.cancel();
                }
                if stop == "lease" {
                    let key: Digest = p.digest().parse().unwrap();
                    let location = layout::lease(&format!("conda-images/{}", key.hex())).unwrap();
                    std::fs::remove_file(store.local_path(&location).unwrap()).unwrap();
                }
                async {
                    if stop == "lease" {
                        Ok(())
                    } else {
                        std::future::pending().await
                    }
                }
            },
        )
        .await;
        assert!(called.get());
        match stop {
            "timeout" => assert!(matches!(
                result,
                Err(CondaImageError::Limit("cache wall-clock"))
            )),
            "cancel" => assert!(matches!(result, Err(CondaImageError::Cancelled))),
            _ => assert!(matches!(result, Err(CondaImageError::Store(_)))),
        }
        assert!(ImageCache::new(store).lookup(&p).await.unwrap().is_none());
        server.join().unwrap();
    }
}

#[tokio::test]
async fn only_proven_invalid_observations_are_removed() {
    for invalid in [true, false] {
        let folder = tempfile::tempdir().unwrap();
        let p = preparation();
        let store = Store::local(&folder.path().join("cache")).unwrap();
        store.put_content(ARCHIVE.as_bytes().into()).await.unwrap();
        store
            .put_entry(
                IMAGE_CACHE_NAMESPACE,
                &p.digest().parse().unwrap(),
                serde_json::to_vec(&receipt(&p)).unwrap().into(),
            )
            .await
            .unwrap();
        let response = if invalid {
            ("/json".into(), 200, "{}".into())
        } else {
            ("/json".into(), 403, "{\"message\":\"denied\"}".into())
        };
        let (engine, server) = engine(folder.path(), vec![response]).await;
        let mut bounded = limits();
        bounded.attempts = 1;
        assert!(
            ensure(
                &engine,
                &p,
                folder.path(),
                &store,
                &Holder::new("cached", "test"),
                bounded,
                &CancellationToken::new()
            )
            .await
            .is_err()
        );
        assert_eq!(
            ImageCache::new(store).lookup(&p).await.unwrap().is_none(),
            invalid
        );
        server.join().unwrap();
    }
}

#[tokio::test]
async fn busy_ownership_and_cancelled_operations_are_bounded() {
    let folder = tempfile::tempdir().unwrap();
    let p = preparation();
    let store = Store::local(&folder.path().join("cache")).unwrap();
    let cache = ImageCache::new(store.clone());
    let claim = cache
        .try_build(&p, &Holder::new("owner", "test"))
        .await
        .unwrap();
    assert!(matches!(claim, ImageBuild::Owner(_)));
    let (engine, server) = engine(folder.path(), vec![]).await;
    let cancel = CancellationToken::new();
    let holder = Holder::new("waiter", "test");
    assert!(matches!(
        ensure(
            &engine,
            &p,
            folder.path(),
            &store,
            &holder,
            limits(),
            &cancel
        )
        .await,
        Err(CondaImageError::Limit("cache attempts"))
    ));
    cancel.cancel();
    assert!(matches!(
        ensure(
            &engine,
            &p,
            folder.path(),
            &store,
            &holder,
            limits(),
            &cancel
        )
        .await,
        Err(CondaImageError::Cancelled)
    ));
    drop(claim);
    server.join().unwrap();
}

#[tokio::test]
async fn already_corrupt_archive_content_cannot_be_published() {
    let folder = tempfile::tempdir().unwrap();
    let p = preparation();
    let store = Store::local(&folder.path().join("cache")).unwrap();
    let digest = store.put_content(ARCHIVE.as_bytes().into()).await.unwrap();
    std::fs::write(
        store.local_path(&layout::content(&digest)).unwrap(),
        "corrupt",
    )
    .unwrap();
    let (engine, server) = engine(
        folder.path(),
        vec![images(&['b']), inspect(&p), inspect(&p), export()],
    )
    .await;
    let result = ensure(
        &engine,
        &p,
        folder.path(),
        &store,
        &Holder::new("corrupt", "test"),
        limits(),
        &CancellationToken::new(),
    )
    .await;
    assert!(matches!(
        result,
        Err(CondaImageError::Store(
            content_store::StoreError::Corrupt { .. }
        ))
    ));
    assert!(ImageCache::new(store).lookup(&p).await.unwrap().is_none());
    server.join().unwrap();
}

#[tokio::test]
async fn local_candidate_limit_stops_before_inspection_or_build() {
    let folder = tempfile::tempdir().unwrap();
    let p = preparation();
    let store = Store::local(&folder.path().join("cache")).unwrap();
    let (engine, server) = engine(folder.path(), vec![images(&['a', 'b'])]).await;
    let mut bounded = limits();
    bounded.candidates = 1;
    assert!(matches!(
        ensure(
            &engine,
            &p,
            folder.path(),
            &store,
            &Holder::new("limited", "test"),
            bounded,
            &CancellationToken::new()
        )
        .await,
        Err(CondaImageError::Limit("local image candidates"))
    ));
    assert!(ImageCache::new(store).lookup(&p).await.unwrap().is_none());
    server.join().unwrap();
}
