//! Checks that a cache hit reads each stored output once and its content only by size.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::PermissionsExt;

use bytes::Bytes;
use content_store::{Store, layout};
use task_runner::{CacheEntry, StoredFile};

#[tokio::test]
async fn reads_outputs_once() {
    let folder = tempfile::tempdir().unwrap();
    let store = Store::local(&folder.path().join("store")).unwrap();
    let made = folder.path().join("out.txt");
    std::fs::write(&made, b"result\n").unwrap();
    let output = store
        .adopt_output("run", "task-1", 1, "out.txt", &made)
        .await
        .unwrap();
    let log = store
        .put_content(Bytes::from_static(b"log\n"))
        .await
        .unwrap();
    let entry = CacheEntry {
        run: "run".into(),
        task: "task-1".into(),
        exit_code: 0,
        files: vec![StoredFile {
            path: "out.txt".into(),
            digest: output.digest.clone(),
            size: output.size,
            location: output.location.clone(),
            directory: false,
        }],
        stdout: Some(log),
        stderr: None,
        milliseconds: None,
        containers: Vec::new(),
    };
    assert!(entry.complete(&store).await.unwrap());
    // A content that cannot be read but has the right size is not read again.
    let content = store.local_path(&layout::content(&output.digest)).unwrap();
    std::fs::remove_file(&content).unwrap();
    std::fs::write(&content, b"result\n").unwrap();
    std::fs::set_permissions(&content, std::fs::Permissions::from_mode(0o000)).unwrap();
    assert!(entry.complete(&store).await.unwrap());
    std::fs::set_permissions(&content, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::write(&content, b"short").unwrap();
    assert!(
        !entry.complete(&store).await.unwrap(),
        "a content of another size"
    );
}
