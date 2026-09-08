//! Logical directory descriptors retain their immutable manifest identity.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use bytes::Bytes;
use content_store::{Store, layout};
use task_model::{Tree, TreeEntry};

#[tokio::test]
async fn descriptors_preserve_empty_and_link_manifests_without_touching_user_members() {
    let folder = tempfile::tempdir().unwrap();
    let local = Store::local(folder.path()).unwrap();
    let shared = Store::memory().unwrap();
    let store = Store::layered(local, shared.clone()).unwrap();
    let location = "runs/run/task/1/work/directory";
    assert_eq!(store.directory(location).await.unwrap(), None);
    let tree = Tree {
        entries: vec![
            TreeEntry::EmptyDirectory {
                path: "empty".into(),
            },
            TreeEntry::Symlink {
                path: "selected".into(),
                target: "empty".into(),
            },
        ],
    };
    let digest = store.put_tree(&tree).await.unwrap();
    store
        .put_output(
            "run",
            "task",
            1,
            "directory.tree",
            Bytes::from_static(b"user"),
        )
        .await
        .unwrap();
    store.put_directory(location, &digest).await.unwrap();
    assert_eq!(
        store.directory(location).await.unwrap(),
        Some(digest.clone())
    );
    assert_eq!(
        shared.directory(location).await.unwrap(),
        Some(digest.clone())
    );
    assert_eq!(shared.get_tree(&digest).await.unwrap(), tree);
    let output = folder.path().join("runs/run/task/1/work/directory.tree");
    assert_eq!(std::fs::read(output).unwrap(), b"user");
    let records = store.directory_records().await.unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].0.location, layout::directory(location));
    assert_eq!(records[0].1.location, location);
    let replacement = shared.put_tree(&Tree::default()).await.unwrap();
    shared.put_directory(location, &replacement).await.unwrap();
    assert_eq!(store.directory(location).await.unwrap(), Some(replacement));
}

#[tokio::test]
async fn legacy_locations_have_no_descriptor_and_corrupt_descriptors_fail_explicitly() {
    let folder = tempfile::tempdir().unwrap();
    let store = Store::local(folder.path()).unwrap();
    let location = "runs/run/task/1/work/directory";
    assert_eq!(store.directory(location).await.unwrap(), None);
    let digest = store.put_tree(&Tree::default()).await.unwrap();
    store.put_directory(location, &digest).await.unwrap();
    std::fs::write(folder.path().join(layout::directory(location)), b"{").unwrap();
    assert!(store.directory(location).await.is_err());
    assert!(store.directory_records().await.is_err());
}

#[tokio::test]
async fn concurrent_layered_downloads_publish_complete_files_without_leftover_parts() {
    let folder = tempfile::tempdir().unwrap();
    let shared = Store::memory().unwrap();
    let body = Bytes::from(vec![b'x'; 1024 * 1024]);
    let location = shared
        .put_output("run", "task", 1, "output", body.clone())
        .await
        .unwrap();
    let store = Store::layered(Store::local(folder.path()).unwrap(), shared).unwrap();
    let start = std::sync::Arc::new(tokio::sync::Barrier::new(16));
    let mut handles = Vec::new();
    for _ in 0..16 {
        let store = store.clone();
        let location = location.clone();
        let start = start.clone();
        handles.push(tokio::spawn(async move {
            start.wait().await;
            store.local_file(&location).await.unwrap().unwrap()
        }));
    }
    for handle in handles {
        let path = handle.await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), body.as_ref());
    }
    let path = store.local_path(&location).unwrap();
    assert_eq!(
        std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
        1
    );
}
