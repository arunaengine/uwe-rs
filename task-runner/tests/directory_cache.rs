//! Directory hits and cleanup retain the complete manifest closure.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::PermissionsExt;

use bytes::Bytes;
use content_store::{Store, layout};
use task_model::{Digest, Tree, TreeEntry};
use task_runner::{CacheEntry, StoredFile};

fn entry(location: &str, digest: Digest, size: u64) -> CacheEntry {
    CacheEntry {
        run: "run".into(),
        task: "task".into(),
        exit_code: 0,
        stdout: None,
        stderr: None,
        milliseconds: None,
        containers: Vec::new(),
        files: vec![StoredFile {
            path: "directory".into(),
            location: location.into(),
            digest,
            size,
            directory: true,
        }],
    }
}

#[tokio::test]
async fn validates_directory_hits() {
    let folder = tempfile::tempdir().unwrap();
    let store = Store::local(folder.path()).unwrap();
    let content = store
        .put_content(Bytes::from_static(b"data"))
        .await
        .unwrap();
    let tree = Tree {
        entries: vec![
            TreeEntry::File {
                path: "a".into(),
                digest: content,
                size: 4,
                executable: false,
            },
            TreeEntry::EmptyDirectory {
                path: "empty".into(),
            },
            TreeEntry::Symlink {
                path: "selected".into(),
                target: "empty".into(),
            },
            TreeEntry::Symlink {
                path: "cycle".into(),
                target: ".".into(),
            },
        ],
    };
    let manifest = store.put_tree(&tree).await.unwrap();
    let location = "runs/run/task/1/work/directory";
    let root = folder.path().join(location);
    std::fs::create_dir_all(root.join("empty")).unwrap();
    std::fs::write(root.join("a"), b"data").unwrap();
    std::os::unix::fs::symlink("empty", root.join("selected")).unwrap();
    std::os::unix::fs::symlink(".", root.join("cycle")).unwrap();
    let entry = entry(location, manifest.clone(), 4);
    assert!(entry.complete(&store).await.unwrap());
    std::fs::write(root.join("empty/unrecorded"), b"extra").unwrap();
    assert!(!entry.complete(&store).await.unwrap());
    std::fs::remove_file(root.join("empty/unrecorded")).unwrap();
    std::fs::write(root.join("unrecorded"), b"extra").unwrap();
    assert!(!entry.complete(&store).await.unwrap());
    std::fs::remove_file(root.join("unrecorded")).unwrap();
    assert!(entry.complete(&store).await.unwrap());
    std::fs::remove_file(root.join("a")).unwrap();
    assert!(root.is_dir() && folder.path().join(layout::tree(&manifest)).is_file());
    assert!(!entry.complete(&store).await.unwrap());
    std::fs::write(root.join("a"), b"wrong").unwrap();
    assert!(!entry.complete(&store).await.unwrap());
    std::fs::write(root.join("a"), b"DATA").unwrap();
    assert!(!entry.complete(&store).await.unwrap());
    std::fs::write(root.join("a"), b"data").unwrap();
    std::fs::remove_dir(root.join("empty")).unwrap();
    assert!(!entry.complete(&store).await.unwrap());
    std::fs::create_dir(root.join("empty")).unwrap();
    std::fs::remove_file(root.join("selected")).unwrap();
    std::os::unix::fs::symlink("./empty", root.join("selected")).unwrap();
    assert!(!entry.complete(&store).await.unwrap());
    std::fs::remove_file(root.join("selected")).unwrap();
    std::os::unix::fs::symlink("empty", root.join("selected")).unwrap();
    assert!(entry.complete(&store).await.unwrap());
    let stored = folder.path().join(layout::tree(&manifest));
    std::fs::set_permissions(&stored, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::write(stored, b"{").unwrap();
    assert!(!entry.complete(&store).await.unwrap());
}

#[tokio::test]
async fn validates_shared_trees() {
    let operator = opendal::Operator::new(opendal::services::Memory::default()).unwrap();
    let shared = Store::remote(operator.clone(), "s3://shared/cache").unwrap();
    let content = shared
        .put_content(Bytes::from_static(b"data"))
        .await
        .unwrap();
    let tree = Tree {
        entries: vec![TreeEntry::File {
            path: "a".into(),
            digest: content,
            size: 4,
            executable: false,
        }],
    };
    let manifest = shared.put_tree(&tree).await.unwrap();
    let location = shared
        .put_output("run", "task", 1, "directory/a", Bytes::from_static(b"data"))
        .await
        .unwrap();
    let directory = location.strip_suffix("/a").unwrap();
    let folder = tempfile::tempdir().unwrap();
    let layered = Store::layered(Store::local(folder.path()).unwrap(), shared.clone()).unwrap();
    let entry = entry(directory, manifest, 4);
    assert!(entry.complete(&layered).await.unwrap());
    assert!(!folder.path().join(directory).exists());
    operator
        .write(&location, Bytes::from_static(b"DATA"))
        .await
        .unwrap();
    assert!(!entry.complete(&layered).await.unwrap());
    operator.delete(&location).await.unwrap();
    assert!(!entry.complete(&shared).await.unwrap());
}

#[tokio::test]
async fn rejects_linked_parents() {
    let folder = tempfile::tempdir().unwrap();
    let store = Store::local(folder.path()).unwrap();
    let content = store
        .put_content(Bytes::from_static(b"same"))
        .await
        .unwrap();
    let tree = Tree {
        entries: ["a/file", "b/file"]
            .map(|path| TreeEntry::File {
                path: path.into(),
                digest: content.clone(),
                size: 4,
                executable: false,
            })
            .into(),
    };
    let manifest = store.put_tree(&tree).await.unwrap();
    let location = "runs/run/task/1/work/directory";
    let root = folder.path().join(location);
    for name in ["a", "b"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
        std::fs::write(root.join(name).join("file"), b"same").unwrap();
    }
    let entry = entry(location, manifest, 8);
    assert!(entry.complete(&store).await.unwrap());
    std::fs::remove_file(root.join("a/file")).unwrap();
    std::fs::remove_dir(root.join("a")).unwrap();
    std::os::unix::fs::symlink("b", root.join("a")).unwrap();
    assert_eq!(std::fs::read(root.join("a/file")).unwrap(), b"same");
    assert!(!entry.complete(&store).await.unwrap());
}

#[tokio::test]
async fn retains_manifest_content() {
    use task_runner::clean::{Keep, plan, remove};
    let folder = tempfile::tempdir().unwrap();
    let store = Store::local(folder.path()).unwrap();
    let content = store
        .put_content(Bytes::from_static(b"kept"))
        .await
        .unwrap();
    let orphan = store
        .put_content(Bytes::from_static(b"orphan"))
        .await
        .unwrap();
    let tree = Tree {
        entries: vec![TreeEntry::File {
            path: "a".into(),
            digest: content.clone(),
            size: 4,
            executable: false,
        }],
    };
    let manifest = store.put_tree(&tree).await.unwrap();
    store
        .put_output(
            "nocache",
            "task",
            1,
            "directory/a",
            Bytes::from_static(b"kept"),
        )
        .await
        .unwrap();
    assert!(store.objects("ac/").await.unwrap().is_empty());
    let keep = Keep {
        runs: ["nocache".into()].into(),
        digests: [manifest.clone()].into(),
        ..Keep::default()
    };
    let planned = plan(&store, &keep).await.unwrap();
    assert!(
        planned
            .objects
            .iter()
            .any(|object| object.location == layout::content(&orphan))
    );
    assert!(
        !planned
            .objects
            .iter()
            .any(|object| object.location == layout::content(&content))
    );
    remove(&store, &planned).await.unwrap();
    let copied = folder.path().join("verified");
    store.materialize_content(&content, &copied).await.unwrap();
    assert_eq!(std::fs::read(copied).unwrap(), b"kept");
    assert!(store.has_tree(&manifest).await.unwrap());
    assert!(!store.has_content(&orphan).await.unwrap());
}

#[tokio::test]
async fn retains_older_members() {
    use task_runner::clean::{Keep, plan};
    let folder = tempfile::tempdir().unwrap();
    let store = Store::local(folder.path()).unwrap();
    let content = store
        .put_content(Bytes::from_static(b"older"))
        .await
        .unwrap();
    std::fs::File::open(folder.path().join(layout::content(&content)))
        .unwrap()
        .set_modified(std::time::SystemTime::UNIX_EPOCH)
        .unwrap();
    let tree = Tree {
        entries: vec![TreeEntry::File {
            path: "a".into(),
            digest: content.clone(),
            size: 5,
            executable: false,
        }],
    };
    store.put_tree(&tree).await.unwrap();
    let keep = Keep {
        used_since: Some(std::time::SystemTime::now() - std::time::Duration::from_secs(1)),
        ..Keep::default()
    };
    let planned = plan(&store, &keep).await.unwrap();
    assert!(
        !planned
            .objects
            .iter()
            .any(|object| object.location == layout::content(&content))
    );
}
