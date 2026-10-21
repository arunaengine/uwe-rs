//! Checks local caching, shared leases and publication across store layers.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use bytes::Bytes;
use content_store::{Holder, Store, layout};
use task_model::Digest;

/// A shared store in memory, as an object store elsewhere would be.
fn shared() -> Store {
    let operator = opendal::Operator::new(opendal::services::Memory::default()).expect("memory");
    Store::remote(operator, "s3://shared/cache").expect("a store")
}

fn layered(folder: &std::path::Path, shared: &Store) -> Store {
    let front = Store::local(folder).expect("a folder");
    Store::layered(front, shared.clone()).expect("a store in layers")
}

#[tokio::test]
async fn writes_both_layers() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let shared = shared();
    let store = layered(folder.path(), &shared);
    let digest = store
        .put_content(Bytes::from_static(b"reads\n"))
        .await
        .expect("puts");
    let location = layout::content(&digest);
    assert!(folder.path().join(&location).is_file());
    assert!(shared.exists(&location).await.expect("asks"));
    let key = Digest::sha256(b"task");
    let entry = Bytes::from_static(b"{\"first\":true}");
    assert!(
        store
            .put_entry("", &key, entry.clone())
            .await
            .expect("puts")
    );
    assert_eq!(
        shared.get_entry("", &key).await.expect("reads"),
        Some(entry)
    );
    // A task output moves into the folder and goes up to the shared store.
    let output = folder.path().join("made.txt");
    std::fs::write(&output, "made\n").expect("writes");
    let stored = (store.adopt_output("run1", "p-1", 1, "made.txt", &output))
        .await
        .expect("adopts");
    assert!(folder.path().join(&stored.location).is_file());
    let copy = shared.get_object(&stored.location).await.expect("reads");
    assert_eq!(copy, Bytes::from_static(b"made\n"));
    assert!(shared.has_content(&stored.digest).await.expect("asks"));
    // The folder has it, so its path is where it is for others.
    let path = folder.path().join(&stored.location);
    assert_eq!(store.url(&stored.location), path.to_string_lossy());
}

#[tokio::test]
async fn caches_shared_downloads() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let shared = shared();
    // Another machine stored these in the shared store only.
    let digest = shared
        .put_content(Bytes::from_static(b"elsewhere\n"))
        .await
        .expect("puts");
    let output = (shared.put_output("run0", "p-1", 1, "out/a.txt", Bytes::from_static(b"a\n")))
        .await
        .expect("puts");
    let key = Digest::sha256(b"task");
    let theirs = Bytes::from_static(b"{\"first\":\"theirs\"}");
    assert!(
        shared
            .put_entry("", &key, theirs.clone())
            .await
            .expect("puts")
    );
    let store = layered(folder.path(), &shared);
    assert!(store.has_content(&digest).await.expect("asks"));
    assert_eq!(
        store.url(&output),
        "s3://shared/cache/runs/run0/p-1/1/work/out/a.txt"
    );
    let bytes = store.get_content(&digest).await.expect("reads");
    assert_eq!(bytes, Bytes::from_static(b"elsewhere\n"));
    assert!(folder.path().join(layout::content(&digest)).is_file());
    // The first result of a key is the one of the shared store, which the folder then keeps.
    let ours = Bytes::from_static(b"{\"first\":\"ours\"}");
    assert!(!store.put_entry("", &key, ours).await.expect("puts"));
    assert_eq!(
        store.get_entry("", &key).await.expect("reads"),
        Some(theirs)
    );
    // A directory output of an object store comes with all its files.
    let directory = "runs/run0/p-1/1/work/out";
    assert!(store.exists(directory).await.expect("asks"));
    let path = store.local_file(directory).await.expect("fetches");
    let path = path.expect("a folder of this machine");
    assert_eq!(
        std::fs::read_to_string(path.join("a.txt")).expect("reads"),
        "a\n"
    );
    assert_eq!(store.location_of(&store.url(&output)), Some(output.clone()));
    assert_eq!(
        store.location_of("s3://shared/cache/runs/run0/p-1/1/work/out/a.txt"),
        Some(output)
    );
}

#[tokio::test]
async fn leases_use_shared() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let shared = shared();
    let store = layered(folder.path(), &shared);
    let holder = Holder::new("run1", "task");
    let lease = store
        .try_lease("tasks/default/k", &holder)
        .await
        .expect("asks");
    assert!(lease.is_some());
    let other = Holder::new("run2", "task");
    let taken = shared
        .try_lease("tasks/default/k", &other)
        .await
        .expect("asks");
    assert!(taken.is_none(), "another machine sees the lease");
    let held = store.held_leases("tasks/").await.expect("lists");
    assert_eq!(held.len(), 1);
}

#[test]
fn requires_folder_front() {
    let error = Store::layered(shared(), shared()).expect_err("an object store in front");
    assert!(error.to_string().contains("folder"), "{error}");
}
