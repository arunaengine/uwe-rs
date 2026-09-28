//! Lease ownership loss, expired takeover and unsupported remote service boundaries.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use content_store::{Holder, Store, StoreError, layout};

#[tokio::test]
async fn detachment_preserves_successor() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::local(root.path()).unwrap();
    let first = store
        .try_lease("tasks/a", &Holder::new("first", "task"))
        .await
        .unwrap()
        .unwrap();
    first.check().await.unwrap();
    assert!(first.is_local());
    std::fs::remove_file(root.path().join(layout::lease("tasks/a").unwrap())).unwrap();
    let replacement = store
        .try_lease("tasks/a", &Holder::new("next", "task"))
        .await
        .unwrap()
        .unwrap();
    assert!(first.check().await.is_err());
    assert_eq!(store.remove_free_leases().await.unwrap(), 0);
    replacement.check().await.unwrap();
    drop(first);
    replacement.check().await.unwrap();
    drop(replacement);
    assert_eq!(store.remove_free_leases().await.unwrap(), 1);
}

#[tokio::test]
async fn expiry_preserves_successor() {
    // Memory is a process-local exception, not evidence of distributed conditional I/O.
    let operator = opendal::Operator::new(opendal::services::Memory::default()).unwrap();
    let store = Store::remote(operator.clone(), "s3://display-name-only").unwrap();
    let location = layout::lease("maintenance").unwrap();
    let first = store
        .try_lease("maintenance", &Holder::new("first", "cleanup"))
        .await
        .unwrap()
        .unwrap();
    first.check().await.unwrap();
    assert!(!first.is_local());
    let mut record: serde_json::Value =
        serde_json::from_slice(&operator.read(&location).await.unwrap().to_bytes()).unwrap();
    record["expires"] = serde_json::json!(0);
    operator
        .write(&location, serde_json::to_vec(&record).unwrap())
        .await
        .unwrap();
    assert!(first.check().await.is_err());
    let next = store
        .try_lease("maintenance", &Holder::new("next", "cleanup"))
        .await
        .unwrap()
        .unwrap();
    assert!(first.check().await.is_err());
    drop(first);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    next.check().await.unwrap();
    assert_eq!(store.remove_free_leases().await.unwrap(), 0);
}

#[tokio::test]
async fn serializes_memory_takeover() {
    let operator = opendal::Operator::new(opendal::services::Memory::default()).unwrap();
    let store = Store::remote(operator.clone(), "memory://test").unwrap();
    let old = store
        .try_lease("tasks/a", &Holder::new("old", "task"))
        .await
        .unwrap()
        .unwrap();
    let location = layout::lease("tasks/a").unwrap();
    let mut record: serde_json::Value =
        serde_json::from_slice(&operator.read(&location).await.unwrap().to_bytes()).unwrap();
    record["expires"] = serde_json::json!(0);
    operator
        .write(&location, serde_json::to_vec(&record).unwrap())
        .await
        .unwrap();
    let mut handles = Vec::new();
    for number in 0..16 {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            store
                .try_lease("tasks/a", &Holder::new(&format!("new-{number}"), "task"))
                .await
                .unwrap()
        }));
    }
    let mut winners = Vec::new();
    for handle in handles {
        winners.extend(handle.await.unwrap());
    }
    assert_eq!(winners.len(), 1);
    assert!(old.check().await.is_err());
    winners[0].check().await.unwrap();
}

#[tokio::test]
async fn requires_conditional_mutations() {
    let root = tempfile::tempdir().unwrap();
    let operator = opendal::Operator::new(
        opendal::services::Fs::default().root(root.path().to_str().unwrap()),
    )
    .unwrap();
    let store = Store::remote(operator, "s3://remote-name-does-not-change-capabilities").unwrap();
    assert!(matches!(
        store.try_lease("maintenance", &Holder::new("run", "cleanup")).await,
        Err(StoreError::Backend(error)) if error.kind() == opendal::ErrorKind::Unsupported
    ));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn front_lacks_authority() {
    let root = tempfile::tempdir().unwrap();
    let store =
        Store::layered(Store::local(root.path()).unwrap(), Store::memory().unwrap()).unwrap();
    let lease = store
        .try_lease("maintenance", &Holder::new("run", "cleanup"))
        .await
        .unwrap()
        .unwrap();
    lease.check().await.unwrap();
    assert!(!lease.is_local());
}

#[tokio::test]
async fn cleanup_preserves_successor() {
    let operator = opendal::Operator::new(opendal::services::Memory::default()).unwrap();
    let store = Store::remote(operator.clone(), "memory://test").unwrap();
    let old = store
        .try_lease("tasks/a", &Holder::new("old", "task"))
        .await
        .unwrap()
        .unwrap();
    let location = layout::lease("tasks/a").unwrap();
    let mut record: serde_json::Value =
        serde_json::from_slice(&operator.read(&location).await.unwrap().to_bytes()).unwrap();
    record["expires"] = serde_json::json!(0);
    operator
        .write(&location, serde_json::to_vec(&record).unwrap())
        .await
        .unwrap();
    let mut handles = Vec::new();
    for number in 0..16 {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            if number % 2 == 0 {
                store.remove_free_leases().await.unwrap();
                None
            } else {
                store
                    .try_lease("tasks/a", &Holder::new("next", "task"))
                    .await
                    .unwrap()
            }
        }));
    }
    let mut winners = Vec::new();
    for handle in handles {
        winners.extend(handle.await.unwrap());
    }
    assert_eq!(winners.len(), 1);
    drop(old);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    winners[0].check().await.unwrap();
}
