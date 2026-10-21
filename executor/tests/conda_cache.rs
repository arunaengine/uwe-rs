//! Cache ownership, verified local reuse, and publication through a bounded fake engine.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

use base64::Engine;
use conda_environment::{
    BuiltImage, CompletedImage, FORMAT, IMAGE_CACHE_NAMESPACE, ImageBuild, ImageCache,
    ImagePreparation, Lock,
};
use content_store::{Holder, Store, layout};
use executor::conda::{BuildLimits, CondaImageError, TransferLimits};
use executor::conda_cache::{CacheContext, CacheLimits, ensure, ensure_with};
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
            let mut auth = None;
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
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("x-registry-auth:") {
                    let offset = line.len() - value.len();
                    auth = Some(line[offset..].trim().to_owned());
                }
            }
            if expected == "/push?" {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(auth.unwrap())
                    .unwrap();
                let auth: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert!(auth["username"].as_str() == Some("push-user"));
                assert!(auth["password"].as_str() == Some("push-password"));
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
async fn reuses_prepared_images() {
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
            CacheContext {
                store: &store,
                holder: &Holder::new(run, "test"),
                registry: None,
            },
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
async fn publishes_verified_archive() {
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
        CacheContext {
            store: &store,
            holder: &Holder::new("cold", "test"),
            registry: None,
        },
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
async fn restores_shared_archive() {
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
        CacheContext {
            store: &store,
            holder: &Holder::new("restore", "test"),
            registry: None,
        },
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
async fn requires_valid_base() {
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
            CacheContext {
                store: &store,
                holder: &Holder::new("cold", "test"),
                registry: None,
            },
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
async fn bounds_owned_preparation() {
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
            CacheContext {
                store: &store,
                holder: &Holder::new("cold", "test"),
                registry: None,
            },
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
async fn removes_invalid_observations() {
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
async fn bounds_busy_ownership() {
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
async fn rejects_corrupt_publication() {
    let folder = tempfile::tempdir().unwrap();
    let p = preparation();
    let store = Store::local(&folder.path().join("cache")).unwrap();
    let digest = store.put_content(ARCHIVE.as_bytes().into()).await.unwrap();
    // Rotten bits keep the size and the read-only mode, so only a read of the object finds
    // them; a write of the same content would repair a damage it can see.
    let stored = store.local_path(&layout::content(&digest)).unwrap();
    let unlocked = std::os::unix::fs::PermissionsExt::from_mode(0o644);
    std::fs::set_permissions(&stored, unlocked).unwrap();
    std::fs::write(&stored, "x".repeat(ARCHIVE.len())).unwrap();
    let locked = std::os::unix::fs::PermissionsExt::from_mode(0o444);
    std::fs::set_permissions(&stored, locked).unwrap();
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
async fn bounds_local_candidates() {
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

fn registry(statuses: Vec<u16>, digest: &str) -> (String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let host = listener.local_addr().unwrap().to_string();
    let digest = digest.to_owned();
    let server = std::thread::spawn(move || {
        let mut challenged = false;
        for status in statuses {
            let deadline = Instant::now() + Duration::from_secs(5);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "missing registry request");
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(line.starts_with("HEAD /v2/tool/manifests/sha256:"));
            let mut authorized = false;
            loop {
                line.clear();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                authorized |= line
                    .to_ascii_lowercase()
                    .starts_with("authorization: basic ");
            }
            if challenged {
                assert!(authorized);
            }
            challenged = status == 401;
            write!(stream, "HTTP/1.1 {status} Response\r\nDocker-Content-Digest: {digest}\r\nWWW-Authenticate: Basic realm=\"registry\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        }
    });
    (format!("{host}/tool"), server)
}

fn pushed(p: &ImagePreparation, registry: &str) -> Reply {
    let mut reply = inspect(p);
    let mut image: serde_json::Value = serde_json::from_str(&reply.2).unwrap();
    image["RepoDigests"] = json!([format!("{registry}@sha256:{}", "c".repeat(64))]);
    reply.2 = image.to_string();
    reply
}

#[tokio::test]
async fn publishes_remote_images() {
    let folder = tempfile::tempdir().unwrap();
    let p = preparation();
    let (registry, remote) = registry(
        vec![401, 200, 401, 200, 404, 401, 200],
        &format!("sha256:{}", "c".repeat(64)),
    );
    let host = registry.split('/').next().unwrap();
    let config = config_file::ConfigFile::parse(
        &format!("[registry.\"{host}\"]\nusername = 'push-user'\npassword = 'push-password'\n"),
        std::path::Path::new("config.toml"),
    )
    .unwrap();
    executor::credentials::configure(&config).unwrap();
    let mut replies = vec![
        images(&['b']),
        inspect(&p),
        inspect(&p),
        export(),
        inspect(&p),
    ];
    for _ in 0..2 {
        replies.extend([
            inspect(&p),
            ("/tag?".into(), 201, String::new()),
            ("/push?".into(), 200, "{\"status\":\"pushed\"}\n".into()),
            pushed(&p, &registry),
        ]);
        if replies.len() == 9 {
            replies.push(inspect(&p));
        }
    }
    let (engine, server) = engine(folder.path(), replies).await;
    let store = Store::local(&folder.path().join("cache")).unwrap();
    let empty = Store::local(&folder.path().join("empty")).unwrap();
    let expected = format!("{registry}@sha256:{}", "c".repeat(64));
    for (index, store) in [&store, &empty, &store].into_iter().enumerate() {
        let image = ensure_with(
            &engine,
            &p,
            &folder.path().join("missing packages"),
            CacheContext {
                store,
                holder: &Holder::new("publish", "test"),
                registry: Some(&registry),
            },
            limits(),
            &CancellationToken::new(),
            || async {
                Err(CondaImageError::Provenance(
                    "registry reuse called preparation".into(),
                ))
            },
        )
        .await
        .unwrap();
        assert_eq!(image.reference(), Some(expected.as_str()));
        assert_eq!(image.image_id(), receipt(&p).image_id());
        if index == 0 {
            empty
                .put_entry(
                    "conda-registry",
                    &p.digest().parse().unwrap(),
                    serde_json::to_vec(&image).unwrap().into(),
                )
                .await
                .unwrap();
            assert!(
                !empty
                    .has_content(&Digest::sha256(ARCHIVE.as_bytes()))
                    .await
                    .unwrap()
            );
        }
    }
    assert!(
        ImageCache::new(store)
            .lookup(&p)
            .await
            .unwrap()
            .unwrap()
            .receipt()
            .reference()
            .is_none()
    );
    server.join().unwrap();
    remote.join().unwrap();
}

#[tokio::test]
async fn refuses_registry_errors() {
    for status in [403, 500, 200] {
        let folder = tempfile::tempdir().unwrap();
        let p = preparation();
        let (registry, remote) = registry(vec![status], &format!("sha256:{}", "d".repeat(64)));
        let store = Store::local(&folder.path().join("cache")).unwrap();
        let mut image = serde_json::to_value(receipt(&p)).unwrap();
        image["reference"] = json!(format!("{registry}@sha256:{}", "c".repeat(64)));
        image["manifest_digest"] = json!(format!("sha256:{}", "c".repeat(64)));
        store
            .put_entry(
                "conda-registry",
                &p.digest().parse().unwrap(),
                serde_json::to_vec(&image).unwrap().into(),
            )
            .await
            .unwrap();
        let (engine, server) = engine(folder.path(), vec![]).await;
        let result = ensure_with(
            &engine,
            &p,
            folder.path(),
            CacheContext {
                store: &store,
                holder: &Holder::new("refused", "test"),
                registry: Some(&registry),
            },
            limits(),
            &CancellationToken::new(),
            || async { panic!("registry errors must not build") },
        )
        .await;
        assert!(matches!(result, Err(CondaImageError::Publication(_))));
        server.join().unwrap();
        remote.join().unwrap();
    }
}

#[tokio::test]
async fn redacts_push_errors() {
    for status in [200, 500] {
        let folder = tempfile::tempdir().unwrap();
        let p = preparation();
        let (engine, server) = engine(folder.path(), vec![
            inspect(&p), ("/tag?".into(), 201, String::new()),
            ("/push".into(), status, "{\"errorDetail\":{\"message\":\"private-value\"},\"message\":\"private-value\"}\n".into()),
        ]).await;
        let error = executor::conda::publish(
            &engine,
            &p,
            &receipt(&p),
            "127.0.0.1:1/tool",
            limits().transfer,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, CondaImageError::Publication(_)));
        assert!(!error.to_string().contains("private-value"));
        server.join().unwrap();
    }
}

#[tokio::test]
#[ignore = "needs a local Debian image, a prepared tiny context and a throwaway registry"]
async fn publishes_local_image() {
    use bollard::query_parameters::{BuildImageOptions, BuilderVersion, RemoveImageOptions};
    use futures_util::StreamExt;

    let base = std::env::var("UWE_PUBLISH_BASE").unwrap();
    let registry = std::env::var("UWE_PUBLISH_REGISTRY").unwrap();
    let context = std::fs::read(std::env::var("UWE_PUBLISH_CONTEXT").unwrap()).unwrap();
    assert!(context.len() <= 16 * 1024);
    let p = ImagePreparation::new(preparation().lock(), &base, "linux/amd64", &[]).unwrap();
    let engine = ContainerEngine::named(Some("docker")).await.unwrap();
    executor::conda::require_base(&engine, &p).await.unwrap();
    let tag = format!("{registry}:build");
    let options = BuildImageOptions {
        t: Some(tag.clone()),
        version: BuilderVersion::BuilderV1,
        forcerm: true,
        platform: "linux/amd64".into(),
        networkmode: Some("none".into()),
        memory: Some(128 << 20),
        memswap: Some(128 << 20),
        cpuperiod: Some(100_000),
        cpuquota: Some(100_000),
        labels: Some(std::collections::HashMap::from([
            ("org.iunctus.conda.preparation".into(), p.digest().into()),
            ("org.iunctus.conda.base".into(), p.base().into()),
            ("org.iunctus.conda.recipe".into(), p.recipe_digest()),
        ])),
        ..Default::default()
    };
    let mut id = None;
    let result = async {
        let mut build =
            engine
                .client()
                .build_image(options, None, Some(bollard::body_full(context.into())));
        while let Some(step) = build.next().await {
            let step = step?;
            if let Some(aux) = step.aux {
                id = aux.id;
            }
        }
        let image = engine.client().inspect_image(&tag).await?;
        id = image.id;
        let image = p.completed(BuiltImage {
            preparation: p.digest().into(),
            base: p.base().into(),
            recipe_digest: p.recipe_digest(),
            platform: p.platform().into(),
            image_id: id.clone().unwrap(),
            manifest_digest: None,
            archive_digest: None,
            reference: None,
        })?;
        let mut bounded = limits();
        bounded.timeout = Duration::from_secs(60);
        bounded.transfer.timeout = Duration::from_secs(60);
        bounded.transfer.progress_bytes = 8 << 20;
        let published = executor::conda::publish(
            &engine,
            &p,
            &image,
            &registry,
            bounded.transfer,
            &CancellationToken::new(),
        )
        .await?;
        let folder = tempfile::tempdir()?;
        let store = Store::local(folder.path())?;
        store
            .put_entry(
                "conda-registry",
                &p.digest().parse().unwrap(),
                serde_json::to_vec(&published).unwrap().into(),
            )
            .await?;
        let reused = ensure_with(
            &engine,
            &p,
            &folder.path().join("missing"),
            CacheContext {
                store: &store,
                holder: &Holder::new("live", "test"),
                registry: Some(&registry),
            },
            bounded,
            &CancellationToken::new(),
            || async { panic!("registry reuse must not build") },
        )
        .await?;
        assert_eq!(published, reused);
        println!("published and reused {}", published.reference().unwrap());
        Ok::<_, CondaImageError>(())
    }
    .await;
    engine
        .client()
        .remove_image(
            id.as_deref().unwrap_or(&tag),
            Some(RemoveImageOptions {
                force: true,
                ..Default::default()
            }),
            None,
        )
        .await
        .unwrap();
    result.unwrap();
}
