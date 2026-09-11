//! Completed Conda images in the shared content store, with one build owner per preparation.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::str::FromStr;
use std::sync::Arc;

use content_store::{EntrySnapshot, Holder, Lease, Store, StoreError};
use task_model::Digest;

use crate::{CompletedImage, ImagePreparation};

/// Cache namespace of completed Conda image receipts.
pub const IMAGE_CACHE_NAMESPACE: &str = "conda-images";

/// A validated receipt and the exact stored entry version from which it was read.
pub struct CachedImage {
    receipt: CompletedImage,
    observed: EntrySnapshot,
    cache: Arc<()>,
}

impl CachedImage {
    pub fn receipt(&self) -> &CompletedImage {
        &self.receipt
    }
}

/// Exclusive build ownership for one preparation. Remote ownership is renewable but is not a
/// transactional fence around a store write, so publication checks it immediately before and
/// after the store's atomic first-writer operation.
pub struct ImageBuildClaim {
    preparation: String,
    lease: Lease,
    cache: Arc<()>,
}

impl ImageBuildClaim {
    pub async fn check(&self) -> Result<(), StoreError> {
        self.lease.check().await
    }
}

/// Result of checking the cache and trying to become the build owner.
pub enum ImageBuild {
    Ready(CachedImage),
    Owner(ImageBuildClaim),
    /// Another caller owns the build. The caller decides its bounded retry and cancellation.
    Busy,
}

/// Completed-image receipts and build ownership in a local, remote or layered store.
#[derive(Clone, Debug)]
pub struct ImageCache {
    store: Store,
    identity: Arc<()>,
}

impl ImageCache {
    pub fn new(store: Store) -> Self {
        // Entries and leases use the shared first-writer authority directly. Front-layer
        // receipt copies may be stale and cannot decide distributed build ownership.
        let mut store = store;
        while let Some(shared) = store.shared() {
            store = shared.clone();
        }
        Self {
            store,
            identity: Arc::new(()),
        }
    }

    /// Finds a valid receipt. Malformed, differently bound, or archive-missing observations are
    /// removed only if their observed version is still current, then lookup is retried.
    pub async fn lookup(
        &self,
        preparation: &ImagePreparation,
    ) -> Result<Option<CachedImage>, StoreError> {
        let key = key(preparation)?;
        for _ in 0..4 {
            let Some(observed) = self
                .store
                .observe_entry(IMAGE_CACHE_NAMESPACE, &key)
                .await?
            else {
                return Ok(None);
            };
            let receipt = serde_json::from_slice::<CompletedImage>(observed.bytes());
            let valid = match receipt {
                Ok(receipt) if receipt.validate(preparation).is_ok() => {
                    if let Some(digest) = receipt.archive_digest() {
                        let digest = digest.parse::<Digest>().map_err(|error| {
                            StoreError::Location(format!("invalid image archive digest: {error}"))
                        })?;
                        self.store.has_content(&digest).await?.then_some(receipt)
                    } else {
                        Some(receipt)
                    }
                }
                _ => None,
            };
            if let Some(receipt) = valid {
                return Ok(Some(CachedImage {
                    receipt,
                    observed,
                    cache: self.identity.clone(),
                }));
            }
            self.store
                .invalidate_entry(IMAGE_CACHE_NAMESPACE, &key, &observed)
                .await?;
        }
        Err(StoreError::Location(
            "completed image entry kept changing during validation".into(),
        ))
    }

    /// Removes this observed receipt only if it is still the stored version.
    pub async fn invalidate(
        &self,
        preparation: &ImagePreparation,
        cached: &CachedImage,
    ) -> Result<bool, StoreError> {
        self.owns(&cached.cache)?;
        self.store
            .invalidate_entry(IMAGE_CACHE_NAMESPACE, &key(preparation)?, &cached.observed)
            .await
    }

    /// Returns a completed receipt, exclusive build ownership, or `Busy`. After taking a lease,
    /// lookup is repeated so an image published just before ownership is used instead of rebuilt.
    pub async fn try_build(
        &self,
        preparation: &ImagePreparation,
        holder: &Holder,
    ) -> Result<ImageBuild, StoreError> {
        if let Some(cached) = self.lookup(preparation).await? {
            return Ok(ImageBuild::Ready(cached));
        }
        let key = key(preparation)?;
        let name = format!("conda-images/{}", key.hex());
        let Some(lease) = self.store.try_lease(&name, holder).await? else {
            return Ok(ImageBuild::Busy);
        };
        let claim = ImageBuildClaim {
            preparation: preparation.digest().into(),
            lease,
            cache: self.identity.clone(),
        };
        if let Some(cached) = self.recheck(preparation, &claim).await? {
            return Ok(ImageBuild::Ready(cached));
        }
        Ok(ImageBuild::Owner(claim))
    }

    /// Rechecks the cache while the caller still owns this preparation's build lease.
    pub async fn recheck(
        &self,
        preparation: &ImagePreparation,
        claim: &ImageBuildClaim,
    ) -> Result<Option<CachedImage>, StoreError> {
        self.owns(&claim.cache)?;
        if claim.preparation != preparation.digest() {
            return Err(StoreError::Location(
                "completed image claim belongs to another preparation".into(),
            ));
        }
        claim.check().await?;
        self.lookup(preparation).await
    }

    /// Publishes a caller-validated build under its claim. A stored archive must already exist.
    /// The first receipt wins; this always rereads and returns that validated winner.
    pub async fn publish(
        &self,
        preparation: &ImagePreparation,
        claim: &ImageBuildClaim,
        receipt: &CompletedImage,
    ) -> Result<CachedImage, StoreError> {
        self.owns(&claim.cache)?;
        if claim.preparation != preparation.digest() {
            return Err(StoreError::Location(
                "completed image claim belongs to another preparation".into(),
            ));
        }
        receipt
            .validate(preparation)
            .map_err(|error| StoreError::Location(error.to_string()))?;
        if let Some(digest) = receipt.archive_digest() {
            let digest = Digest::from_str(digest)
                .map_err(|error| StoreError::Location(error.to_string()))?;
            if !self.store.has_content(&digest).await? {
                return Err(StoreError::Location(
                    "completed image archive is not in the content store".into(),
                ));
            }
        }
        claim.check().await?;
        let bytes = serde_json::to_vec(receipt).map_err(std::io::Error::other)?;
        self.store
            .put_entry(IMAGE_CACHE_NAMESPACE, &key(preparation)?, bytes.into())
            .await?;
        claim.check().await?;
        self.lookup(preparation).await?.ok_or_else(|| {
            StoreError::Location("completed image winner disappeared after publication".into())
        })
    }

    fn owns(&self, identity: &Arc<()>) -> Result<(), StoreError> {
        if !Arc::ptr_eq(&self.identity, identity) {
            return Err(StoreError::Location(
                "completed image cache value belongs to another cache".into(),
            ));
        }
        Ok(())
    }
}

fn key(preparation: &ImagePreparation) -> Result<Digest, StoreError> {
    preparation
        .digest()
        .parse()
        .map_err(|error| StoreError::Location(format!("invalid preparation digest: {error}")))
}
