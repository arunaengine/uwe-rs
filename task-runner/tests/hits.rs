//! Checks that a cache hit reads each stored output once and its content only by size.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::PermissionsExt;

use bytes::Bytes;
use content_store::{Store, layout};
use opendal::raw::*;
use opendal::{OperationContext, Operator};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use task_runner::{CacheEntry, StoredFile};

#[derive(Debug)]
struct Counted {
    inner: Servicer,
    reads: Mutex<BTreeMap<String, usize>>,
}

impl Service for Counted {
    type Reader = oio::Reader;
    type Writer = oio::Writer;
    type Lister = oio::Lister;
    type Deleter = oio::Deleter;
    type Copier = oio::Copier;
    type Composer = oio::Composer;

    fn info(&self) -> ServiceInfo {
        self.inner.info()
    }
    fn capability(&self) -> opendal::Capability {
        self.inner.capability()
    }
    async fn create_dir(
        &self,
        ctx: &OperationContext,
        path: &str,
        args: OpCreateDir,
    ) -> opendal::Result<RpCreateDir> {
        self.inner.create_dir(ctx, path, args).await
    }
    async fn stat(
        &self,
        ctx: &OperationContext,
        path: &str,
        args: OpStat,
    ) -> opendal::Result<RpStat> {
        self.inner.stat(ctx, path, args).await
    }
    fn read(
        &self,
        ctx: &OperationContext,
        path: &str,
        args: OpRead,
    ) -> opendal::Result<Self::Reader> {
        *self
            .reads
            .lock()
            .unwrap()
            .entry(path.to_owned())
            .or_default() += 1;
        self.inner.read(ctx, path, args)
    }
    fn write(
        &self,
        ctx: &OperationContext,
        path: &str,
        args: OpWrite,
    ) -> opendal::Result<Self::Writer> {
        self.inner.write(ctx, path, args)
    }
    fn delete(&self, ctx: &OperationContext) -> opendal::Result<Self::Deleter> {
        self.inner.delete(ctx)
    }
    fn list(
        &self,
        ctx: &OperationContext,
        path: &str,
        args: OpList,
    ) -> opendal::Result<Self::Lister> {
        self.inner.list(ctx, path, args)
    }
    fn copy(
        &self,
        ctx: &OperationContext,
        from: &str,
        to: &str,
        args: OpCopy,
    ) -> opendal::Result<Self::Copier> {
        self.inner.copy(ctx, from, to, args)
    }
    fn compose(
        &self,
        ctx: &OperationContext,
        to: &str,
        args: OpCompose,
    ) -> opendal::Result<Self::Composer> {
        self.inner.compose(ctx, to, args)
    }
}

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

#[tokio::test]
async fn verifies_overlaps_once() {
    let inner = Operator::new(opendal::services::Memory::default()).unwrap();
    let counted = Arc::new(Counted {
        inner: inner.service().clone(),
        reads: Default::default(),
    });
    let store = Store::remote(
        Operator::from_parts(inner.context().clone(), counted.clone()),
        "test://outputs",
    )
    .unwrap();
    let output = store
        .put_output(
            "run",
            "task",
            1,
            "directory/file",
            Bytes::from_static(b"result"),
        )
        .await
        .unwrap();
    let tree = task_model::Tree {
        entries: vec![task_model::TreeEntry::File {
            path: "file".into(),
            digest: output.digest.clone(),
            size: output.size,
            executable: false,
        }],
    };
    let digest = store.put_tree(&tree).await.unwrap();
    let log = store.put_content(Bytes::from_static(b"log")).await.unwrap();
    let entry = CacheEntry {
        run: "run".into(),
        task: "task".into(),
        exit_code: 0,
        milliseconds: None,
        stdout: Some(log.clone()),
        stderr: Some(log.clone()),
        containers: Vec::new(),
        files: vec![
            StoredFile {
                path: "directory".into(),
                location: "runs/run/task/1/work/directory".into(),
                digest,
                size: output.size,
                directory: true,
            },
            StoredFile {
                path: "directory/file".into(),
                location: output.location.clone(),
                digest: output.digest,
                size: output.size,
                directory: false,
            },
        ],
    };
    counted.reads.lock().unwrap().clear();
    assert!(entry.complete(&store).await.unwrap());
    let reads = counted.reads.lock().unwrap();
    assert_eq!(reads[&output.location], 1);
    assert_eq!(reads[&layout::content(&log)], 1);
    drop(reads);
    inner
        .write(&output.location, b"broken".as_slice())
        .await
        .unwrap();
    assert!(!entry.complete(&store).await.unwrap());
}
