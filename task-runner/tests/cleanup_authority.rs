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

#[tokio::test]
async fn bounded_retention_matches() {
    use task_model::Digest;
    use task_runner::clean::{Summary, scan};
    use task_runner::{CacheEntry, StoredFile};
    let target = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/remediation/cleanup-tests");
    std::fs::create_dir_all(&target).unwrap();
    let root = tempfile::tempdir_in(target).unwrap();
    let store = Store::local(&root.path().join("store")).unwrap();
    let scratch = root.path().join("scratch");
    let mut obsolete = Vec::new();
    for (run, content) in [("kept", b"keep"), ("old", b"gone")] {
        let source = root.path().join(run);
        std::fs::write(&source, content).unwrap();
        let output = store
            .adopt_output(run, "task", 1, "output", &source)
            .await
            .unwrap();
        let key = Digest::sha256(run.as_bytes());
        let entry = CacheEntry {
            run: run.into(),
            task: "task".into(),
            exit_code: 0,
            files: vec![StoredFile {
                path: "output".into(),
                digest: output.digest.clone(),
                size: output.size,
                location: output.location.clone(),
                directory: false,
            }],
            stdout: None,
            stderr: None,
            milliseconds: Some(1),
            containers: vec![],
        };
        store
            .put_entry("", &key, entry.to_bytes(&key).unwrap().into())
            .await
            .unwrap();
        if run == "old" {
            obsolete = vec![
                content_store::layout::content(&output.digest),
                output.location,
            ];
        }
    }
    let keep = Keep {
        runs: ["kept".into()].into(),
        ..Keep::default()
    };
    let report = scan(&store, &keep, &scratch).await.unwrap();
    assert_eq!(
        report.summary,
        Summary {
            stored_results: 2,
            stored_objects: 4,
            stored_bytes: 8,
            results: 1,
            objects: 2,
            freed: 4,
            contents: 1
        }
    );
    let mut groups = Vec::new();
    report
        .visit_runs(|run, results, objects| groups.push((run.to_owned(), results, objects)))
        .unwrap();
    assert_eq!(groups, [("old".into(), 1, 1)]);
    let legacy = plan(&store, &keep).await.unwrap();
    let mut locations: Vec<_> = legacy
        .objects
        .iter()
        .map(|object| object.location.clone())
        .collect();
    locations.sort();
    obsolete.sort();
    assert_eq!(locations, obsolete);
    assert_eq!(legacy.freed(), report.freed());
    assert_eq!(legacy.results[0].run.as_deref(), Some("old"));
    drop(report);
    assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
    let guard = maintenance(&store).await.unwrap();
    let removed = task_runner::clean::apply_guarded(&store, &keep, &scratch, &guard)
        .await
        .unwrap();
    assert_eq!((removed.results, removed.objects), (1, 2));
    assert!(scan(&store, &keep, &scratch).await.unwrap().is_empty());
}

#[tokio::test]
async fn bounded_rechecks_readers() {
    use content_store::Holder;
    use task_runner::clean::{apply_guarded, scan};
    let target = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/remediation/cleanup-tests");
    std::fs::create_dir_all(&target).unwrap();
    let root = tempfile::tempdir_in(target).unwrap();
    let store = Store::local(&root.path().join("store")).unwrap();
    let scratch = root.path().join("scratch");
    let digest = store
        .put_content(Bytes::from_static(b"keep"))
        .await
        .unwrap();
    assert_eq!(
        scan(&store, &Keep::default(), &scratch)
            .await
            .unwrap()
            .summary
            .objects,
        1
    );
    let _reader = store
        .try_lease("runs/reader", &Holder::new("reader", "run"))
        .await
        .unwrap()
        .unwrap();
    let guard = maintenance(&store).await.unwrap();
    let removed = apply_guarded(&store, &Keep::default(), &scratch, &guard)
        .await
        .unwrap();
    assert_eq!(removed.objects, 0);
    assert!(store.has_content(&digest).await.unwrap());
    let remote = Store::memory().unwrap();
    let guard = maintenance(&remote).await.unwrap();
    let absent = root.path().join("unsupported");
    assert!(
        apply_guarded(&remote, &Keep::default(), &absent, &guard)
            .await
            .is_err()
    );
    assert!(!absent.exists());
}

#[tokio::test]
async fn bounded_keeps_directories() {
    use task_model::{Tree, TreeEntry};
    use task_runner::clean::scan;
    let target = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/remediation/cleanup-tests");
    std::fs::create_dir_all(&target).unwrap();
    let root = tempfile::tempdir_in(target).unwrap();
    let store = Store::local(&root.path().join("store")).unwrap();
    let content = store.put_content("data".into()).await.unwrap();
    let orphan = store.put_content("gone".into()).await.unwrap();
    let manifest = store
        .put_tree(&Tree {
            entries: vec![TreeEntry::File {
                path: "child".into(),
                digest: content.clone(),
                size: 4,
                executable: false,
            }],
        })
        .await
        .unwrap();
    let location = store
        .put_output("old", "task", 1, "directory/child", "data".into())
        .await
        .unwrap();
    store
        .put_directory("runs/old/task/1/work/directory", &manifest)
        .await
        .unwrap();
    for keep in [
        Keep {
            locations: [location].into(),
            ..Keep::default()
        },
        Keep {
            digests: [manifest.clone()].into(),
            ..Keep::default()
        },
    ] {
        let scratch = root.path().join("scratch");
        let report = scan(&store, &keep, &scratch).await.unwrap();
        assert_eq!((report.summary.results, report.summary.objects), (0, 1));
        assert_eq!(report.summary.freed, 4);
        let legacy = plan(&store, &keep).await.unwrap();
        assert_eq!(
            legacy.objects[0].location,
            content_store::layout::content(&orphan)
        );
        assert_eq!(legacy.freed(), report.freed());
    }
}
