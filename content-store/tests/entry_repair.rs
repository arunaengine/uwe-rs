//! Cache repair removes only the observed stale entry, while eviction stays local.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use bytes::Bytes;
use content_store::{Store, StoreError};
use task_model::Digest;

async fn repair(store: &Store) {
    let key = Digest::sha256(b"stale task");
    let stale = Bytes::from_static(b"stale");
    let repaired = Bytes::from_static(b"repaired");
    assert!(store.put_entry("", &key, stale.clone()).await.unwrap());
    assert_eq!(
        store.get_entry("", &key).await.unwrap(),
        Some(stale.clone())
    );
    let observed = store.observe_entry("", &key).await.unwrap().unwrap();
    assert_eq!(observed.bytes(), &stale);
    assert!(store.invalidate_entry("", &key, &observed).await.unwrap());
    assert!(store.put_entry("", &key, repaired.clone()).await.unwrap());
    // A late invalidator must not delete the first worker's repaired successor.
    assert!(!store.invalidate_entry("", &key, &observed).await.unwrap());
    assert_eq!(
        store.get_entry("", &key).await.unwrap(),
        Some(repaired.clone())
    );
    if let Some(shared) = store.shared() {
        assert_eq!(shared.get_entry("", &key).await.unwrap(), Some(repaired));
    }
}

#[tokio::test]
async fn stale_entries_are_repaired_without_deleting_successors() {
    let root = tempfile::tempdir().unwrap();
    for shared in [
        Store::memory().unwrap(),
        Store::local(&root.path().join("shared")).unwrap(),
    ] {
        let front = tempfile::tempdir().unwrap();
        repair(&Store::layered(Store::local(front.path()).unwrap(), shared).unwrap()).await;
    }
    repair(&Store::memory().unwrap()).await;
    repair(&Store::local(&root.path().join("single")).unwrap()).await;
}

#[tokio::test]
async fn local_eviction_does_not_invalidate_shared_entry() {
    let root = tempfile::tempdir().unwrap();
    let store =
        Store::layered(Store::local(root.path()).unwrap(), Store::memory().unwrap()).unwrap();
    let key = Digest::sha256(b"evicted task");
    let entry = Bytes::from_static(b"valid");
    store.put_entry("", &key, entry.clone()).await.unwrap();
    store.remove_entry("", &key).await.unwrap();
    assert_eq!(
        store.shared().unwrap().get_entry("", &key).await.unwrap(),
        Some(entry.clone())
    );
    assert_eq!(store.get_entry("", &key).await.unwrap(), Some(entry));
}

#[tokio::test]
async fn mismatched_front_does_not_invalidate_shared_repair() {
    let root = tempfile::tempdir().unwrap();
    let shared = Store::memory().unwrap();
    let store = Store::layered(Store::local(root.path()).unwrap(), shared.clone()).unwrap();
    let key = Digest::sha256(b"concurrent task");
    store
        .put_entry("", &key, Bytes::from_static(b"old"))
        .await
        .unwrap();
    let old = shared.observe_entry("", &key).await.unwrap().unwrap();
    assert!(shared.invalidate_entry("", &key, &old).await.unwrap());
    let repaired = Bytes::from_static(b"new");
    assert!(shared.put_entry("", &key, repaired.clone()).await.unwrap());
    let front = store.observe_entry("", &key).await.unwrap().unwrap();
    assert_eq!(front.bytes(), b"old".as_slice());
    assert!(store.invalidate_entry("", &key, &front).await.unwrap());
    assert_eq!(
        store.get_entry("", &key).await.unwrap(),
        Some(repaired.clone())
    );
    assert_eq!(shared.get_entry("", &key).await.unwrap(), Some(repaired));
}

#[tokio::test]
async fn shared_repair_works_without_a_front_copy() {
    let root = tempfile::tempdir().unwrap();
    let shared = Store::memory().unwrap();
    let key = Digest::sha256(b"uncached task");
    shared
        .put_entry("", &key, Bytes::from_static(b"old"))
        .await
        .unwrap();
    let store = Store::layered(Store::local(root.path()).unwrap(), shared).unwrap();
    let old = store.observe_entry("", &key).await.unwrap().unwrap();
    // A reader can hydrate the old front after the invalidator observed its absence.
    assert_eq!(
        store.get_entry("", &key).await.unwrap(),
        Some(Bytes::from_static(b"old"))
    );
    assert!(store.invalidate_entry("", &key, &old).await.unwrap());
    assert!(
        store
            .put_entry("", &key, Bytes::from_static(b"new"))
            .await
            .unwrap()
    );
    assert_eq!(
        store.get_entry("", &key).await.unwrap(),
        Some(Bytes::from_static(b"new"))
    );
}

#[tokio::test]
async fn nonconditional_remote_recovery_fails_without_deleting_entry() {
    let root = tempfile::tempdir().unwrap();
    let operator = opendal::Operator::new(
        opendal::services::Fs::default().root(root.path().to_str().unwrap()),
    )
    .unwrap();
    let store = Store::remote(operator, "test://cache").unwrap();
    let key = Digest::sha256(b"remote task");
    let entry = Bytes::from_static(b"stale");
    store.put_entry("", &key, entry.clone()).await.unwrap();
    let observed = store.observe_entry("", &key).await.unwrap().unwrap();
    let error = store
        .invalidate_entry("", &key, &observed)
        .await
        .unwrap_err();
    assert!(
        matches!(error, StoreError::Backend(error) if error.kind() == opendal::ErrorKind::Unsupported)
    );
    assert_eq!(store.get_entry("", &key).await.unwrap(), Some(entry));
}

#[tokio::test]
async fn local_file_identity_retains_an_identical_byte_successor() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::local(root.path()).unwrap();
    let key = Digest::sha256(b"local entry");
    let entry = Bytes::from_static(b"unchanged");
    store.put_entry("", &key, entry.clone()).await.unwrap();
    let old = store.observe_entry("", &key).await.unwrap().unwrap();
    store.remove_entry("", &key).await.unwrap();
    store.put_entry("", &key, entry.clone()).await.unwrap();
    assert!(!store.invalidate_entry("", &key, &old).await.unwrap());
    assert_eq!(store.get_entry("", &key).await.unwrap(), Some(entry));
}
