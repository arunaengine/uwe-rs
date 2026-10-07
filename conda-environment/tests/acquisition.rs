//! Cached acquisition and one opt-in HTTPS smoke test.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use conda_environment::{
    Acquisition, Environment, Lock, LockError, Package, repodata_path, valid_channel,
};
use sha2::{Digest as _, Sha256};

fn fixture(bytes: &[u8]) -> Lock {
    let digest: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Lock {
        format: conda_environment::FORMAT,
        platform: "linux-64".into(),
        channels: vec![],
        specs: vec![],
        pip: vec![],
        pypi: None,
        virtual_packages: Some(vec![]),
        packages: vec![Package {
            name: "fixture".into(),
            version: "1".into(),
            build: "0".into(),
            channel: "test".into(),
            subdir: "noarch".into(),
            file: "fixture-1-0.conda".into(),
            url: "https://invalid.example/fixture-1-0.conda".into(),
            sha256: Some(digest),
            md5: None,
        }],
    }
}

#[test]
fn contains_channel_paths() {
    let root = std::path::Path::new("/cache");
    let first = repodata_path(root, "https://example.org/channel", "linux-64");
    assert!(first.starts_with(root));
    assert_eq!(
        first,
        repodata_path(root, "https://example.org/channel/", "linux-64")
    );
    assert_ne!(
        first,
        repodata_path(root, "https://other.example/channel", "linux-64")
    );
    assert!(valid_channel("https://example.org/channel"));
    for channel in [
        "http://example.org/channel",
        "https://user:pass@example.org/channel",
        "https://example.org/channel?token=secret",
        "../outside",
    ] {
        assert!(!valid_channel(channel));
    }
}

#[tokio::test]
async fn verifies_cached_packages() {
    let cache = tempfile::tempdir().unwrap();
    let bytes = b"verified package bytes";
    let lock = fixture(bytes);
    let path = cache.path().join(lock.packages[0].sha256.as_ref().unwrap());
    std::fs::write(&path, bytes).unwrap();
    for offline in [true, false] {
        let acquisition = Acquisition::new(cache.path(), offline).unwrap();
        assert_eq!(
            acquisition
                .fetch_packages(&lock, cache.path())
                .await
                .unwrap(),
            std::slice::from_ref(&path)
        );
    }
    std::fs::write(&path, b"corrupt").unwrap();
    let acquisition = Acquisition::new(cache.path(), true).unwrap();
    assert!(
        acquisition
            .fetch_packages(&lock, cache.path())
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(path).unwrap(), b"corrupt");
    let environment =
        Environment::parse("channels: [conda-forge]\ndependencies: [tzdata]\n").unwrap();
    assert!(matches!(
        acquisition.lock(&environment, "linux-64", Some(&[])).await,
        Err(LockError::Acquisition(_))
    ));
}

#[tokio::test]
#[ignore = "requires HTTPS access to conda-forge; serial repodata and package smoke test"]
async fn reuses_offline_acquisitions() {
    let cache = tempfile::tempdir().unwrap();
    let acquisition = Acquisition::new(cache.path(), false).unwrap();
    let environment =
        Environment::parse("channels: [conda-forge]\ndependencies: [tzdata]\n").unwrap();
    let lock = acquisition
        .lock(&environment, "linux-64", Some(&[]))
        .await
        .unwrap();
    assert_eq!(lock.packages.len(), 1);
    let packages = cache.path().join("packages");
    let paths = acquisition.fetch_packages(&lock, &packages).await.unwrap();
    assert!(!std::fs::read(&paths[0]).unwrap().is_empty());
    let offline = Acquisition::new(cache.path(), true).unwrap();
    let cached = offline
        .lock(&environment, "linux-64", Some(&[]))
        .await
        .unwrap();
    assert_eq!(cached.digest(), lock.digest());
    assert_eq!(
        offline.fetch_packages(&cached, &packages).await.unwrap(),
        paths
    );
    println!(
        "locked and verified {} from HTTPS, then reused repodata and package bytes offline",
        lock.packages[0].file
    );
}
