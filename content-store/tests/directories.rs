//! Logical directory descriptors retain their immutable manifest identity.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use bytes::Bytes;
use content_store::{Store, layout};
use task_model::{Digest, Tree, TreeEntry};

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

#[tokio::test]
async fn child_first_hydration_returns_the_complete_manifest_without_replacing_user_members() {
    use std::os::unix::fs::PermissionsExt;
    let folder = tempfile::tempdir().unwrap();
    let shared = Store::memory().unwrap();
    let a = shared.put_content(Bytes::from_static(b"a")).await.unwrap();
    let b = shared.put_content(Bytes::from_static(b"b")).await.unwrap();
    let location = "runs/run/task/1/work/directory";
    let child = shared
        .put_output("run", "task", 1, "directory/a", Bytes::from_static(b"a"))
        .await
        .unwrap();
    let tree = Tree {
        entries: vec![
            TreeEntry::File {
                path: "a".into(),
                digest: a,
                size: 1,
                executable: false,
            },
            TreeEntry::File {
                path: "nested/b".into(),
                digest: b,
                size: 1,
                executable: true,
            },
            TreeEntry::EmptyDirectory {
                path: "empty".into(),
            },
            TreeEntry::Symlink {
                path: "selected".into(),
                target: "nested".into(),
            },
            TreeEntry::Symlink {
                path: "nested/alias".into(),
                target: "../empty".into(),
            },
        ],
    };
    let digest = shared.put_tree(&tree).await.unwrap();
    shared.put_directory(location, &digest).await.unwrap();
    let store = Store::layered(Store::local(folder.path()).unwrap(), shared.clone()).unwrap();
    store.local_file(&child).await.unwrap();
    let logical = folder.path().join(location);
    std::fs::write(logical.join("user"), b"preserved").unwrap();
    let path = store.local_file(location).await.unwrap().unwrap();
    assert_eq!(
        path,
        folder
            .path()
            .join(layout::directory_snapshot(&digest))
            .join("input")
    );
    assert_eq!(std::fs::read(path.join("a")).unwrap(), b"a");
    assert_eq!(std::fs::read(path.join("selected/b")).unwrap(), b"b");
    assert!(path.join("nested/alias").is_dir());
    assert_eq!(std::fs::read_dir(path.join("empty")).unwrap().count(), 0);
    assert_ne!(
        std::fs::metadata(path.join("nested/b"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    );
    assert_eq!(std::fs::read(logical.join("user")).unwrap(), b"preserved");
    assert!(!logical.join("nested").exists());
    let replacement = shared.put_tree(&Tree::default()).await.unwrap();
    shared.put_directory(location, &replacement).await.unwrap();
    let empty = store.local_file(location).await.unwrap().unwrap();
    assert_ne!(empty, path);
    assert_eq!(std::fs::read_dir(empty).unwrap().count(), 0);
    assert_eq!(std::fs::read(path.join("a")).unwrap(), b"a");
}

#[tokio::test]
async fn failed_member_transfer_does_not_publish_a_directory_and_retry_is_complete() {
    let folder = tempfile::tempdir().unwrap();
    let operator = opendal::Operator::new(opendal::services::Memory::default()).unwrap();
    let shared = Store::remote(operator.clone(), "s3://shared/cache").unwrap();
    let a = shared.put_content(Bytes::from_static(b"a")).await.unwrap();
    let b = Digest::sha256(b"b");
    let tree = Tree {
        entries: vec![
            TreeEntry::File {
                path: "a".into(),
                digest: a,
                size: 1,
                executable: false,
            },
            TreeEntry::File {
                path: "b".into(),
                digest: b.clone(),
                size: 1,
                executable: false,
            },
        ],
    };
    let digest = shared.put_tree(&tree).await.unwrap();
    let location = "runs/run/task/1/work/directory";
    shared.put_directory(location, &digest).await.unwrap();
    let store = Store::layered(Store::local(folder.path()).unwrap(), shared.clone()).unwrap();
    let target = folder.path().join(layout::directory_snapshot(&digest));
    assert!(store.local_file(location).await.is_err());
    assert!(!target.exists());
    assert_eq!(
        std::fs::read_dir(target.parent().unwrap()).unwrap().count(),
        0
    );
    operator
        .write(&layout::content(&b), Bytes::from_static(b"x"))
        .await
        .unwrap();
    assert!(matches!(
        store.local_file(location).await,
        Err(content_store::StoreError::Corrupt { .. })
    ));
    assert_eq!(
        std::fs::read_dir(target.parent().unwrap()).unwrap().count(),
        0
    );
    operator
        .write(&layout::content(&b), Bytes::from_static(b"b"))
        .await
        .unwrap();
    let path = store.local_file(location).await.unwrap().unwrap();
    assert_eq!(std::fs::read(path.join("a")).unwrap(), b"a");
    assert_eq!(std::fs::read(path.join("b")).unwrap(), b"b");
    std::fs::remove_file(path.join("b")).unwrap();
    assert!(store.local_file(location).await.is_err());
}

#[tokio::test]
async fn concurrent_directory_readers_only_receive_the_same_complete_tree() {
    let folder = tempfile::tempdir().unwrap();
    let shared = Store::memory().unwrap();
    let content = Bytes::from(vec![b'x'; 1024 * 1024]);
    let file = shared.put_content(content.clone()).await.unwrap();
    let tree = Tree {
        entries: vec![
            TreeEntry::File {
                path: "first".into(),
                digest: file.clone(),
                size: content.len() as u64,
                executable: false,
            },
            TreeEntry::File {
                path: "second".into(),
                digest: file,
                size: content.len() as u64,
                executable: false,
            },
            TreeEntry::EmptyDirectory {
                path: "empty".into(),
            },
        ],
    };
    let digest = shared.put_tree(&tree).await.unwrap();
    let location = "runs/run/task/1/work/directory";
    shared.put_directory(location, &digest).await.unwrap();
    let store = Store::layered(Store::local(folder.path()).unwrap(), shared).unwrap();
    let start = std::sync::Arc::new(tokio::sync::Barrier::new(8));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let start = start.clone();
        handles.push(tokio::spawn(async move {
            start.wait().await;
            store.local_file(location).await.unwrap().unwrap()
        }));
    }
    let expected = folder
        .path()
        .join(layout::directory_snapshot(&digest))
        .join("input");
    for handle in handles {
        let path = handle.await.unwrap();
        assert_eq!(path, expected);
        assert_eq!(std::fs::read(path.join("first")).unwrap(), content.as_ref());
        assert_eq!(
            std::fs::read(path.join("second")).unwrap(),
            content.as_ref()
        );
        assert!(path.join("empty").is_dir());
    }
    assert_eq!(
        std::fs::read_dir(expected.parent().unwrap().parent().unwrap())
            .unwrap()
            .count(),
        1
    );
}

#[tokio::test]
async fn manifests_reject_outside_links_cycles_conflicting_paths_and_unlisted_members() {
    let folder = tempfile::tempdir().unwrap();
    let shared = Store::memory().unwrap();
    let store = Store::layered(Store::local(folder.path()).unwrap(), shared.clone()).unwrap();
    let location = "runs/run/task/1/work/directory";
    for entries in [
        vec![TreeEntry::Symlink {
            path: "selected".into(),
            target: "../outside".into(),
        }],
        vec![TreeEntry::Symlink {
            path: "selected".into(),
            target: "selected".into(),
        }],
        vec![
            TreeEntry::EmptyDirectory {
                path: "empty".into(),
            },
            TreeEntry::EmptyDirectory {
                path: "empty/child".into(),
            },
        ],
        vec![TreeEntry::EmptyDirectory {
            path: "../outside".into(),
        }],
    ] {
        let digest = shared.put_tree(&Tree { entries }).await.unwrap();
        shared.put_directory(location, &digest).await.unwrap();
        assert!(store.local_file(location).await.is_err());
        assert!(
            !folder
                .path()
                .join(layout::directory_snapshot(&digest))
                .exists()
        );
    }
    let digest = shared.put_tree(&Tree::default()).await.unwrap();
    shared.put_directory(location, &digest).await.unwrap();
    assert!(store.exists(location).await.unwrap());
    let path = store.local_file(location).await.unwrap().unwrap();
    std::fs::write(path.join("unlisted"), b"extra").unwrap();
    assert!(store.local_file(location).await.is_err());
}

#[tokio::test]
async fn legacy_child_first_remote_and_local_directories_are_also_copied_privately() {
    for local_shared in [false, true] {
        let folder = tempfile::tempdir().unwrap();
        let shared_folder = tempfile::tempdir().unwrap();
        let shared = if local_shared {
            Store::local(shared_folder.path()).unwrap()
        } else {
            Store::memory().unwrap()
        };
        let child = shared
            .put_output("run", "task", 1, "directory/a", Bytes::from_static(b"a"))
            .await
            .unwrap();
        shared
            .put_output("run", "task", 1, "directory/b", Bytes::from_static(b"b"))
            .await
            .unwrap();
        let store = Store::layered(Store::local(folder.path()).unwrap(), shared).unwrap();
        store.local_file(&child).await.unwrap();
        let location = "runs/run/task/1/work/directory";
        let path = store.local_file(location).await.unwrap().unwrap();
        assert!(path.starts_with(folder.path().join("directory-snapshots/legacy")));
        assert_eq!(std::fs::read(path.join("a")).unwrap(), b"a");
        assert_eq!(std::fs::read(path.join("b")).unwrap(), b"b");
        assert!(!folder.path().join(location).join("b").exists());
    }
}

mod authority {
    use super::*;
    use opendal::raw::*;
    use opendal::{ErrorKind, OperationContext, Operator};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct Unavailable {
        calls: Arc<AtomicUsize>,
        offline: bool,
        fail_list: bool,
        folder_error: ErrorKind,
    }

    impl Service for Unavailable {
        type Reader = ();
        type Writer = ();
        type Lister = ();
        type Deleter = ();
        type Copier = ();
        type Composer = ();

        fn info(&self) -> ServiceInfo {
            ServiceInfo::new("hydration-test", "/", "test")
        }
        fn capability(&self) -> opendal::Capability {
            opendal::Capability {
                read: true,
                stat: true,
                list: true,
                list_with_recursive: true,
                ..Default::default()
            }
        }
        async fn create_dir(
            &self,
            _: &OperationContext,
            _: &str,
            _: OpCreateDir,
        ) -> opendal::Result<RpCreateDir> {
            unreachable!()
        }
        async fn stat(
            &self,
            _: &OperationContext,
            path: &str,
            _: OpStat,
        ) -> opendal::Result<RpStat> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let kind = if path.ends_with('/') {
                self.folder_error
            } else {
                ErrorKind::NotFound
            };
            Err(opendal::Error::new(
                kind,
                "authoritative directory stat failed",
            ))
        }
        fn read(&self, _: &OperationContext, _: &str, _: OpRead) -> opendal::Result<Self::Reader> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let kind = if self.offline {
                ErrorKind::Unexpected
            } else {
                ErrorKind::NotFound
            };
            Err(opendal::Error::new(kind, "shared read unavailable"))
        }
        fn write(
            &self,
            _: &OperationContext,
            _: &str,
            _: OpWrite,
        ) -> opendal::Result<Self::Writer> {
            unreachable!()
        }
        fn delete(&self, _: &OperationContext) -> opendal::Result<Self::Deleter> {
            unreachable!()
        }
        fn list(&self, _: &OperationContext, _: &str, _: OpList) -> opendal::Result<Self::Lister> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_list {
                Err(opendal::Error::new(
                    ErrorKind::Unexpected,
                    "authoritative directory listing failed",
                ))
            } else {
                Ok(())
            }
        }
        fn copy(
            &self,
            _: &OperationContext,
            _: &str,
            _: &str,
            _: OpCopy,
        ) -> opendal::Result<Self::Copier> {
            unreachable!()
        }
        async fn rename(
            &self,
            _: &OperationContext,
            _: &str,
            _: &str,
            _: OpRename,
        ) -> opendal::Result<RpRename> {
            unreachable!()
        }
        async fn presign(
            &self,
            _: &OperationContext,
            _: &str,
            _: OpPresign,
        ) -> opendal::Result<RpPresign> {
            unreachable!()
        }
    }

    fn shared(
        offline: bool,
        fail_list: bool,
        folder_error: ErrorKind,
    ) -> (Store, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let inner = Operator::new(opendal::services::Memory::default()).unwrap();
        let service = Arc::new(Unavailable {
            calls: calls.clone(),
            offline,
            fail_list,
            folder_error,
        });
        let operator = Operator::from_parts(inner.context().clone(), service);
        (
            Store::remote(operator, "test://unavailable").unwrap(),
            calls,
        )
    }

    #[tokio::test]
    async fn existing_regular_files_need_no_shared_access_while_offline() {
        let folder = tempfile::tempdir().unwrap();
        let front = Store::local(folder.path()).unwrap();
        let digest = front
            .put_content(Bytes::from_static(b"front CAS"))
            .await
            .unwrap();
        let output = front
            .put_output(
                "run",
                "task",
                1,
                "file",
                Bytes::from_static(b"front output"),
            )
            .await
            .unwrap();
        let (shared, calls) = shared(true, true, ErrorKind::Unexpected);
        let store = Store::layered(front, shared).unwrap();
        for (location, expected) in [
            (layout::content(&digest), b"front CAS".as_slice()),
            (output, b"front output".as_slice()),
        ] {
            let path = store.local_file(&location).await.unwrap().unwrap();
            assert_eq!(std::fs::read(path).unwrap(), expected);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn absent_or_failed_authoritative_trees_never_accept_a_partial_front_root() {
        for (fail_list, folder_error) in [
            (false, ErrorKind::NotFound),
            (false, ErrorKind::Unexpected),
            (true, ErrorKind::NotFound),
        ] {
            let folder = tempfile::tempdir().unwrap();
            let location = "runs/run/task/1/work/directory";
            let partial = folder.path().join(location);
            std::fs::create_dir_all(&partial).unwrap();
            std::fs::write(partial.join("child"), b"partial").unwrap();
            let (shared, _) = shared(false, fail_list, folder_error);
            let store = Store::layered(Store::local(folder.path()).unwrap(), shared).unwrap();
            let error = store.local_file(location).await.unwrap_err();
            assert_eq!(
                error.missing(),
                !fail_list && folder_error == ErrorKind::NotFound
            );
            assert_eq!(std::fs::read(partial.join("child")).unwrap(), b"partial");
            assert_eq!(
                std::fs::read_dir(folder.path().join("directory-snapshots/legacy"))
                    .unwrap()
                    .count(),
                0
            );
        }
    }
}
