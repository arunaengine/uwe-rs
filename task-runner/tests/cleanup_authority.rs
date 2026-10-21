//! Destructive cleanup requires non-expiring authority, including local-front stores.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use bytes::Bytes;
use content_store::Store;
use task_runner::clean::{Keep, maintenance, plan, remove, remove_guarded};

#[tokio::test]
async fn cleanup_requires_lock() {
    let folder = tempfile::tempdir().unwrap();
    let store = Store::local(folder.path()).unwrap();
    let content = store
        .put_content(Bytes::from_static(b"remove"))
        .await
        .unwrap();
    let planned = plan(&store, &Keep::default()).await.unwrap();
    let guard = maintenance(&store).await.unwrap();
    guard.ensure_cleanup().unwrap();
    assert!(
        remove_guarded(&store, &planned, &guard)
            .await
            .unwrap()
            .objects
            > 0
    );
    assert!(!store.has_content(&content).await.unwrap());
}

#[tokio::test]
async fn rejects_expiring_authority() {
    for front in [false, true] {
        let shared = Store::memory().unwrap();
        let folder = tempfile::tempdir().unwrap();
        let store = if front {
            Store::layered(Store::local(folder.path()).unwrap(), shared.clone()).unwrap()
        } else {
            shared.clone()
        };
        let content = store
            .put_content(Bytes::from_static(b"keep"))
            .await
            .unwrap();
        let planned = plan(&store, &Keep::default()).await.unwrap();
        let guard = maintenance(&store).await.unwrap();
        assert!(
            guard
                .ensure_cleanup()
                .unwrap_err()
                .to_string()
                .contains("non-expiring")
        );
        assert!(
            remove_guarded(&store, &planned, &guard)
                .await
                .unwrap_err()
                .to_string()
                .contains("non-expiring")
        );
        assert_eq!(
            store.get_content(&content).await.unwrap(),
            b"keep".as_slice()
        );
        assert!(shared.has_content(&content).await.unwrap());
    }
    let store = Store::memory().unwrap();
    let content = store
        .put_content(Bytes::from_static(b"legacy"))
        .await
        .unwrap();
    let planned = plan(&store, &Keep::default()).await.unwrap();
    assert!(
        remove(&store, &planned)
            .await
            .unwrap_err()
            .to_string()
            .contains("non-expiring")
    );
    assert_eq!(
        store.get_content(&content).await.unwrap(),
        b"legacy".as_slice()
    );
}

#[tokio::test]
async fn rejects_remote_cleanup() {
    let store = Store::memory().unwrap();
    let planned = plan(&store, &Keep::default()).await.unwrap();
    assert!(planned.is_empty());
    let guard = maintenance(&store).await.unwrap();
    assert!(guard.ensure_cleanup().is_err());
    assert!(remove_guarded(&store, &planned, &guard).await.is_err());
}
