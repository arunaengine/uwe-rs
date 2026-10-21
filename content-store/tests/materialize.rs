//! Checks streaming restoration of verified content without partial target publication.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::PermissionsExt;

use bytes::Bytes;
use content_store::{Store, StoreError, layout};
use task_model::Digest;

#[tokio::test]
async fn rejects_oversized_content() {
    let front = tempfile::tempdir().unwrap();
    let shared = Store::memory().unwrap();
    let digest = shared
        .put_content(Bytes::from_static(b"larger"))
        .await
        .unwrap();
    let store = Store::layered(Store::local(front.path()).unwrap(), shared).unwrap();
    let folder = tempfile::tempdir().unwrap();
    let target = folder.path().join("target");
    std::fs::write(&target, b"old").unwrap();
    for path in [&target, &folder.path().join("missing/target")] {
        let error = store
            .materialize_content_bounded(&digest, path, 5)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("byte limit"), "{error}");
    }
    assert_eq!(std::fs::read(&target).unwrap(), b"old");
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 1);
    assert_eq!(std::fs::read_dir(front.path()).unwrap().count(), 0);
    store
        .materialize_content_bounded(&digest, &target, 6)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"larger");
}

#[tokio::test]
async fn verifies_bounded_content() {
    let folder = tempfile::tempdir().unwrap();
    let operator = opendal::Operator::new(opendal::services::Memory::default()).unwrap();
    let store = Store::remote(operator.clone(), "s3://shared/cache").unwrap();
    let digest = Digest::sha256(b"good");
    operator
        .write(&layout::content(&digest), Bytes::from_static(b"bad!"))
        .await
        .unwrap();
    let target = folder.path().join("target");
    assert!(matches!(
        store.materialize_content_bounded(&digest, &target, 4).await,
        Err(StoreError::Corrupt { .. })
    ));
    assert!(!target.exists());
    let empty = store.put_content(Bytes::new()).await.unwrap();
    store
        .materialize_content_bounded(&empty, &target, 0)
        .await
        .unwrap();
    assert_eq!(std::fs::metadata(&target).unwrap().len(), 0);
}

#[tokio::test]
async fn materializes_multiple_parts() {
    let operator = opendal::Operator::new(opendal::services::Memory::default()).unwrap();
    let store = Store::remote(operator, "s3://shared/cache").unwrap();
    let content: Vec<u8> = (0..9 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let digest = store
        .put_content(Bytes::from(content.clone()))
        .await
        .unwrap();
    let folder = tempfile::tempdir().unwrap();
    let target = folder.path().join("nested/content");
    store.materialize_content(&digest, &target).await.unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), content);
    assert_eq!(
        std::fs::read_dir(target.parent().unwrap()).unwrap().count(),
        1
    );
}

#[tokio::test]
async fn materializes_caches_shared() {
    let folder = tempfile::tempdir().unwrap();
    for shared in [
        Store::memory().unwrap(),
        Store::local(&folder.path().join("shared")).unwrap(),
    ] {
        let front = tempfile::tempdir().unwrap();
        let digest = shared
            .put_content(Bytes::from_static(b"shared"))
            .await
            .unwrap();
        let store = Store::layered(Store::local(front.path()).unwrap(), shared).unwrap();
        let target = folder.path().join("target");
        store.materialize_content(&digest, &target).await.unwrap();
        let cached = front.path().join(layout::content(&digest));
        assert_eq!(std::fs::read(&cached).unwrap(), b"shared");
        assert_eq!(std::fs::read(&target).unwrap(), b"shared");
        store
            .shared()
            .unwrap()
            .remove(&layout::content(&digest))
            .await
            .unwrap();
        std::fs::write(&target, b"old").unwrap();
        store.materialize_content(&digest, &target).await.unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"shared");
    }
}

#[tokio::test]
async fn corruption_preserves_target() {
    let folder = tempfile::tempdir().unwrap();
    let operator = opendal::Operator::new(opendal::services::Memory::default()).unwrap();
    let store = Store::remote(operator.clone(), "s3://shared/cache").unwrap();
    let digest = Digest::sha256(b"good");
    let existing = folder.path().join("existing");
    std::fs::write(&existing, b"old").unwrap();
    let missing = folder.path().join("missing");
    for target in [&existing, &missing] {
        // Each failed read removes the damaged object.
        operator
            .write(&layout::content(&digest), Bytes::from_static(b"bad"))
            .await
            .unwrap();
        assert!(matches!(
            store.materialize_content(&digest, target).await,
            Err(StoreError::Corrupt { .. })
        ));
    }
    assert_eq!(std::fs::read(&existing).unwrap(), b"old");
    assert!(!missing.exists());
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn rejects_corrupt_shared() {
    let folder = tempfile::tempdir().unwrap();
    let shared_folder = tempfile::tempdir().unwrap();
    let shared = Store::local(shared_folder.path()).unwrap();
    let digest = shared
        .put_content(Bytes::from_static(b"good"))
        .await
        .unwrap();
    damage(&shared_folder.path().join(layout::content(&digest)), b"bad");
    let store = Store::layered(Store::local(folder.path()).unwrap(), shared).unwrap();
    let target = folder.path().join("target");
    assert!(matches!(
        store.materialize_content(&digest, &target).await,
        Err(StoreError::Corrupt { .. })
    ));
    assert!(!target.exists());
    assert!(!folder.path().join(layout::content(&digest)).exists());
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn rejects_corrupt_local() {
    let folder = tempfile::tempdir().unwrap();
    let shared = Store::memory().unwrap();
    let store = Store::layered(Store::local(folder.path()).unwrap(), shared).unwrap();
    let digest = store
        .put_content(Bytes::from_static(b"good"))
        .await
        .unwrap();
    damage(&folder.path().join(layout::content(&digest)), b"bad");
    let target = folder.path().join("target");
    std::fs::write(&target, b"old").unwrap();
    assert!(matches!(
        store.materialize_content(&digest, &target).await,
        Err(StoreError::Corrupt { .. })
    ));
    assert_eq!(std::fs::read(&target).unwrap(), b"old");
}

#[tokio::test]
async fn missing_preserves_target() {
    let folder = tempfile::tempdir().unwrap();
    let target = folder.path().join("target");
    std::fs::write(&target, b"old").unwrap();
    let store = Store::memory().unwrap();
    assert!(
        store
            .materialize_content(&Digest::sha256(b"missing"), &target)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"old");
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn publication_preserves_target() {
    let folder = tempfile::tempdir().unwrap();
    let target = folder.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("old"), b"old").unwrap();
    let store = Store::memory().unwrap();
    let digest = store.put_content(Bytes::from_static(b"new")).await.unwrap();
    assert!(store.materialize_content(&digest, &target).await.is_err());
    assert_eq!(std::fs::read(target.join("old")).unwrap(), b"old");
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn materializes_empty_content() {
    let folder = tempfile::tempdir().unwrap();
    let target = folder.path().join("target");
    let store = Store::memory().unwrap();
    let digest = store.put_content(Bytes::new()).await.unwrap();
    store.materialize_content(&digest, &target).await.unwrap();
    assert_eq!(std::fs::metadata(&target).unwrap().len(), 0);
}

#[tokio::test]
async fn isolates_concurrent_temporaries() {
    let folder = tempfile::tempdir().unwrap();
    let store = Store::memory().unwrap();
    let digest = store
        .put_content(Bytes::from_static(b"same"))
        .await
        .unwrap();
    let targets: Vec<_> = (0..16).map(|i| folder.path().join(i.to_string())).collect();
    let mut handles = Vec::new();
    for target in &targets {
        let store = store.clone();
        let digest = digest.clone();
        let target = target.clone();
        handles.push(tokio::spawn(async move {
            store.materialize_content(&digest, &target).await
        }));
    }
    for handle in handles {
        handle.await.unwrap().unwrap();
    }
    for target in targets {
        assert_eq!(std::fs::read(&target).unwrap(), b"same");
    }
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 16);
}

#[tokio::test]
async fn repairs_corrupt_content() {
    let folder = tempfile::tempdir().unwrap();
    let store = Store::local(folder.path()).unwrap();
    let good = Bytes::from_static(b"good");
    let digest = store.put_content(good.clone()).await.unwrap();
    let stored = folder.path().join(layout::content(&digest));
    // Bits that rot keep the size and the mode, so only a read finds them.
    let rot = || {
        damage(&stored, b"gone");
        std::fs::set_permissions(&stored, std::fs::Permissions::from_mode(0o444)).unwrap();
    };
    rot();
    assert!(matches!(
        store.get_content(&digest).await,
        Err(StoreError::Corrupt { .. })
    ));
    assert!(!stored.exists(), "a damaged object goes");
    store.put_content(good.clone()).await.unwrap();
    assert_eq!(store.get_content(&digest).await.unwrap(), good);
    rot();
    let target = folder.path().join("target");
    assert!(matches!(
        store.materialize_content(&digest, &target).await,
        Err(StoreError::Corrupt { .. })
    ));
    store.put_content(good.clone()).await.unwrap();
    store.materialize_content(&digest, &target).await.unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"good");
}

/// Changes a stored file the way a user can: unlock it, then write other bytes.
fn damage(path: &std::path::Path, bytes: &[u8]) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::write(path, bytes).unwrap();
}
