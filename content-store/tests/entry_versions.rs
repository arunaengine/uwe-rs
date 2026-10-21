//! Conditional remote reads bind the bytes used for validation to the deletion token.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use content_store::{Store, layout};
use opendal::raw::*;
use opendal::{ErrorKind, OperationContext, Operator};
use task_model::Digest;

#[derive(Debug)]
struct Versions {
    inner: Servicer,
    version: Arc<AtomicUsize>,
    reads: AtomicUsize,
    stats: AtomicUsize,
    unavailable: bool,
}

impl Service for Versions {
    type Reader = oio::Reader;
    type Writer = oio::Writer;
    type Lister = oio::Lister;
    type Deleter = ConditionalDelete;
    type Copier = oio::Copier;
    type Composer = oio::Composer;

    fn info(&self) -> ServiceInfo {
        ServiceInfo::new("entry-version-test", "/", "test")
    }
    fn capability(&self) -> opendal::Capability {
        let mut capability = self.inner.capability();
        capability.read_with_if_match = true;
        capability.delete_with_if_match = true;
        capability
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
        self.stats.fetch_add(1, Ordering::SeqCst);
        if self.unavailable {
            return Err(opendal::Error::new(
                ErrorKind::Unexpected,
                "shared store unavailable",
            ));
        }
        let tag = self.version.load(Ordering::SeqCst).to_string();
        Ok(self
            .inner
            .stat(ctx, path, args)
            .await?
            .map_metadata(|metadata| {
                let mut metadata = metadata.into_builder();
                metadata.etag(tag);
                metadata.build()
            }))
    }
    fn read(
        &self,
        ctx: &OperationContext,
        path: &str,
        args: OpRead,
    ) -> opendal::Result<Self::Reader> {
        if self.reads.fetch_add(1, Ordering::SeqCst) == 0 {
            // The first read races a replacement after stat returned its old tag.
            self.version.fetch_add(1, Ordering::SeqCst);
        }
        let tag = self.version.load(Ordering::SeqCst).to_string();
        if args.if_match() != Some(tag.as_str()) {
            return Err(opendal::Error::new(
                ErrorKind::ConditionNotMatch,
                "entry changed after stat",
            ));
        }
        self.inner.read(ctx, path, OpRead::new())
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
        Ok(ConditionalDelete {
            inner: self.inner.delete(ctx)?,
            version: self.version.clone(),
        })
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
    async fn rename(
        &self,
        ctx: &OperationContext,
        from: &str,
        to: &str,
        args: OpRename,
    ) -> opendal::Result<RpRename> {
        self.inner.rename(ctx, from, to, args).await
    }
    async fn presign(
        &self,
        ctx: &OperationContext,
        path: &str,
        args: OpPresign,
    ) -> opendal::Result<RpPresign> {
        self.inner.presign(ctx, path, args).await
    }
}

struct ConditionalDelete {
    inner: oio::Deleter,
    version: Arc<AtomicUsize>,
}

impl oio::Delete for ConditionalDelete {
    async fn delete(&mut self, path: &str, args: OpDelete) -> opendal::Result<()> {
        let tag = self.version.load(Ordering::SeqCst).to_string();
        if args.if_match() != Some(tag.as_str()) {
            return Err(opendal::Error::new(
                ErrorKind::ConditionNotMatch,
                "entry changed after read",
            ));
        }
        self.inner.delete(path, OpDelete::new()).await
    }
    async fn close(&mut self) -> opendal::Result<()> {
        self.inner.close().await
    }
}

#[tokio::test]
async fn invalidation_preserves_versions() {
    let inner = Operator::new(opendal::services::Memory::default()).unwrap();
    let key = Digest::sha256(b"remote entry");
    let location = layout::entry("", &key).unwrap();
    inner.write(&location, "old").await.unwrap();
    let service = Arc::new(Versions {
        inner: inner.service().clone(),
        version: Arc::new(AtomicUsize::new(0)),
        reads: AtomicUsize::new(0),
        stats: AtomicUsize::new(0),
        unavailable: false,
    });
    let operator = Operator::from_parts(inner.context().clone(), service.clone());
    let store = Store::remote(operator, "test://entries").unwrap();
    let old = store.observe_entry("", &key).await.unwrap().unwrap();
    assert_eq!(old.bytes().as_ref(), b"old");
    assert!(service.reads.load(Ordering::SeqCst) >= 2);
    inner.write(&location, "new").await.unwrap();
    service.version.fetch_add(1, Ordering::SeqCst);
    assert!(!store.invalidate_entry("", &key, &old).await.unwrap());
    assert_eq!(
        inner.read(&location).await.unwrap().to_bytes().as_ref(),
        b"new"
    );
    let new = store.observe_entry("", &key).await.unwrap().unwrap();
    assert!(store.invalidate_entry("", &key, &new).await.unwrap());
    assert!(!inner.exists(&location).await.unwrap());
}

#[tokio::test]
async fn reports_shared_outage() {
    let root = tempfile::tempdir().unwrap();
    let front = Store::local(root.path()).unwrap();
    let key = Digest::sha256(b"offline entry");
    let bytes = bytes::Bytes::from_static(b"valid front");
    front.put_entry("", &key, bytes.clone()).await.unwrap();
    let inner = Operator::new(opendal::services::Memory::default()).unwrap();
    let service = Arc::new(Versions {
        inner: inner.service().clone(),
        version: Arc::new(AtomicUsize::new(0)),
        reads: AtomicUsize::new(0),
        stats: AtomicUsize::new(0),
        unavailable: true,
    });
    let operator = Operator::from_parts(inner.context().clone(), service.clone());
    let shared = Store::remote(operator, "test://unavailable").unwrap();
    let store = Store::layered(front.clone(), shared).unwrap();
    let observed = store.observe_entry("", &key).await.unwrap().unwrap();
    assert_eq!(observed.bytes(), &bytes);
    assert_eq!(service.stats.load(Ordering::SeqCst), 0);
    let error = store
        .invalidate_entry("", &key, &observed)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("shared store unavailable"));
    assert_eq!(service.stats.load(Ordering::SeqCst), 1);
    assert_eq!(front.get_entry("", &key).await.unwrap(), Some(bytes));
}
