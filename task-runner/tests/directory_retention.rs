//! Bare directory locations retain descriptors, manifests and their complete CAS closure.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use bytes::Bytes;
use content_store::{Store, layout};
use task_model::{Tree, TreeEntry};
use task_runner::clean::{Keep, plan, remove};

#[tokio::test]
async fn retains_directory_closure() {
    for selected in 0..3 {
        let folder = tempfile::tempdir().unwrap();
        let store = Store::local(folder.path()).unwrap();
        let content = store
            .put_content(Bytes::from_static(b"member"))
            .await
            .unwrap();
        let tree = Tree {
            entries: vec![
                TreeEntry::File {
                    path: "a".into(),
                    digest: content.clone(),
                    size: 6,
                    executable: false,
                },
                TreeEntry::Symlink {
                    path: "cycle".into(),
                    target: ".".into(),
                },
            ],
        };
        let manifest = store.put_tree(&tree).await.unwrap();
        let location = "runs/nocache/task/1/work/directory";
        store
            .put_output(
                "nocache",
                "task",
                1,
                "directory/a",
                Bytes::from_static(b"member"),
            )
            .await
            .unwrap();
        store.put_directory(location, &manifest).await.unwrap();
        std::os::unix::fs::symlink(".", folder.path().join(location).join("cycle")).unwrap();
        let snapshot = folder
            .path()
            .join(layout::directory_snapshot(&manifest))
            .join("input");
        std::fs::create_dir_all(&snapshot).unwrap();
        std::fs::write(snapshot.join("a"), b"member").unwrap();
        std::os::unix::fs::symlink(".", snapshot.join("cycle")).unwrap();
        let keep = match selected {
            0 => Keep {
                locations: [location.to_owned()].into(),
                ..Keep::default()
            },
            1 => Keep {
                runs: ["nocache".to_owned()].into(),
                ..Keep::default()
            },
            _ => Keep {
                digests: [manifest.clone()].into(),
                ..Keep::default()
            },
        };
        let planned = plan(&store, &keep).await.unwrap();
        assert!(planned.objects.is_empty(), "{selected}: {planned:?}");
        remove(&store, &planned).await.unwrap();
        assert!(store.has_content(&content).await.unwrap());
        assert!(store.has_tree(&manifest).await.unwrap());
        assert!(store.exists(&layout::directory(location)).await.unwrap());
        assert!(snapshot.join("a").is_file());
        assert_eq!(
            std::fs::read_link(snapshot.join("cycle")).unwrap(),
            std::path::Path::new(".")
        );
        let unkept = plan(&store, &Keep::default()).await.unwrap();
        assert!(
            unkept
                .objects
                .iter()
                .any(|object| object.location == layout::directory(location))
        );
        remove(&store, &unkept).await.unwrap();
        assert_eq!(store.directory(location).await.unwrap(), None);
        assert!(!store.has_tree(&manifest).await.unwrap());
        assert!(!store.has_content(&content).await.unwrap());
        assert!(!snapshot.exists());
    }
}
