//! Checks streaming restoration of verified content without partial target publication.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use bytes::Bytes;
use content_store::{Store, StoreError, layout};
use task_model::Digest;

#[tokio::test]
async fn bounded_content_preserves_targets_and_does_not_cache_oversized_bytes() {
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
async fn bounded_content_still_verifies_digest_and_accepts_empty_bytes() {
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
async fn materializes_remote_content_larger_than_one_part() {
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
async fn local_and_shared_content_are_materialized_and_shared_content_is_cached() {
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
async fn corrupt_content_preserves_targets_and_leaves_no_temporary_files() {
    let folder = tempfile::tempdir().unwrap();
    let operator = opendal::Operator::new(opendal::services::Memory::default()).unwrap();
    let store = Store::remote(operator.clone(), "s3://shared/cache").unwrap();
    let digest = Digest::sha256(b"good");
    operator
        .write(&layout::content(&digest), Bytes::from_static(b"bad"))
        .await
        .unwrap();
    let existing = folder.path().join("existing");
    std::fs::write(&existing, b"old").unwrap();
    let missing = folder.path().join("missing");
    for target in [&existing, &missing] {
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
async fn corrupt_shared_content_is_not_cached() {
    let folder = tempfile::tempdir().unwrap();
    let shared_folder = tempfile::tempdir().unwrap();
    let shared = Store::local(shared_folder.path()).unwrap();
    let digest = shared
        .put_content(Bytes::from_static(b"good"))
        .await
        .unwrap();
    std::fs::write(shared_folder.path().join(layout::content(&digest)), b"bad").unwrap();
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
async fn corrupt_local_content_does_not_fall_back_to_a_healthy_shared_copy() {
    let folder = tempfile::tempdir().unwrap();
    let shared = Store::memory().unwrap();
    let store = Store::layered(Store::local(folder.path()).unwrap(), shared).unwrap();
    let digest = store
        .put_content(Bytes::from_static(b"good"))
        .await
        .unwrap();
    std::fs::write(folder.path().join(layout::content(&digest)), b"bad").unwrap();
    let target = folder.path().join("target");
    std::fs::write(&target, b"old").unwrap();
    assert!(matches!(
        store.materialize_content(&digest, &target).await,
        Err(StoreError::Corrupt { .. })
    ));
    assert_eq!(std::fs::read(&target).unwrap(), b"old");
}

#[tokio::test]
async fn missing_content_preserves_an_existing_target() {
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
async fn failed_publication_preserves_target_and_cleans_temporary_file() {
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
async fn empty_content_can_be_materialized() {
    let folder = tempfile::tempdir().unwrap();
    let target = folder.path().join("target");
    let store = Store::memory().unwrap();
    let digest = store.put_content(Bytes::new()).await.unwrap();
    store.materialize_content(&digest, &target).await.unwrap();
    assert_eq!(std::fs::metadata(&target).unwrap().len(), 0);
}

#[tokio::test]
async fn concurrent_materializations_use_separate_temporary_files() {
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
