//! Bounded reuse and publication of completed Conda images across workflow runs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::time::{Duration, Instant};

use bollard::query_parameters::ListImagesOptions;
use conda_environment::{CompletedImage, ImageBuild, ImageCache, ImagePreparation};
use content_store::{Holder, Store, StoreError};

use crate::conda::{self, BuildLimits, CondaImageError, TransferLimits};
use crate::{CancellationToken, ContainerEngine};

/// One total deadline and finite ownership attempts, in addition to operation limits.
#[derive(Clone, Copy, Debug)]
pub struct CacheLimits {
    pub build: BuildLimits,
    pub transfer: TransferLimits,
    pub timeout: Duration,
    pub attempts: usize,
    pub retry_delay: Duration,
    pub candidates: usize,
}

/// The shared cache store and the owner of a preparation attempt.
pub struct CacheContext<'a> {
    pub store: &'a Store,
    pub holder: &'a Holder,
    pub registry: Option<&'a str>,
}

/// Reuses a verified cache winner or compatible local image before building. Receipts and
/// archives use the deepest shared store. Lease checks surround work and publication but
/// are not an atomic distributed fence; an interrupted publication may have succeeded.
pub async fn ensure(
    engine: &ContainerEngine,
    preparation: &ImagePreparation,
    package_cache: &Path,
    store: &Store,
    holder: &Holder,
    limits: CacheLimits,
    cancel: &CancellationToken,
) -> Result<CompletedImage, CondaImageError> {
    ensure_with(
        engine,
        preparation,
        package_cache,
        CacheContext {
            store,
            holder,
            registry: None,
        },
        limits,
        cancel,
        || async { Ok(()) },
    )
    .await
}

/// Prepares prerequisites only before a cold build, after receipt and local-image reuse.
/// The callback shares the total deadline and cancellation and is checked for ownership
/// before and after it runs. Dropping it cannot undo external work already submitted.
pub async fn ensure_with<F, Fut>(
    engine: &ContainerEngine,
    preparation: &ImagePreparation,
    package_cache: &Path,
    context: CacheContext<'_>,
    limits: CacheLimits,
    cancel: &CancellationToken,
    mut prepare: F,
) -> Result<CompletedImage, CondaImageError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), CondaImageError>>,
{
    let CacheContext {
        store,
        holder,
        registry,
    } = context;
    if limits.attempts == 0
        || limits.candidates == 0
        || limits.retry_delay.is_zero()
        || limits.timeout.is_zero()
        || Instant::now().checked_add(limits.timeout).is_none()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "cache limits must be positive and finite",
        )
        .into());
    }
    let mut store = store;
    while let Some(shared) = store.shared() {
        store = shared;
    }
    let cache = ImageCache::new(store.clone());
    let work = async {
        let key = preparation
            .digest()
            .parse::<task_model::Digest>()
            .map_err(std::io::Error::other)?;
        if let Some(registry) = registry
            && let Some(observed) = store.observe_entry("conda-registry", &key).await?
        {
            if let Ok(receipt) = serde_json::from_slice::<CompletedImage>(observed.bytes())
                && receipt.validate(preparation).is_ok()
                && let Some(reference) = receipt.reference()
                && crate::registry::Reference::parse(reference).name == registry
                && crate::registry::exists(reference)
                    .await
                    .map_err(|error| CondaImageError::Publication(error.to_string()))?
            {
                return Ok(receipt);
            }
            store
                .invalidate_entry("conda-registry", &key, &observed)
                .await?;
        }
        for _ in 0..limits.attempts {
            let cached = match cache.try_build(preparation, holder).await? {
                ImageBuild::Busy => {
                    tracing::debug!("Conda image cache is busy; waiting to retry");
                    tokio::time::sleep(limits.retry_delay).await;
                    continue;
                }
                ImageBuild::Ready(cached) => {
                    tracing::debug!("Conda image cache hit");
                    cached
                }
                ImageBuild::Owner(claim) => {
                    tracing::debug!("Conda image cache ownership acquired");
                    if let Some(cached) = cache.recheck(preparation, &claim).await? {
                        cached
                    } else {
                        claim.check().await?;
                        let existing = local(engine, preparation, limits.candidates).await?;
                        if let Some(cached) = cache.recheck(preparation, &claim).await? {
                            cached
                        } else {
                            let image = match existing {
                                Some(image) => image,
                                None => {
                                    claim.check().await?;
                                    prepare().await?;
                                    claim.check().await?;
                                    conda::build(
                                        engine,
                                        preparation,
                                        package_cache,
                                        limits.build,
                                        cancel,
                                    )
                                    .await?
                                }
                            };
                            claim.check().await?;
                            if let Some(cached) = cache.recheck(preparation, &claim).await? {
                                cached
                            } else {
                                let saved = conda::save(
                                    engine,
                                    preparation,
                                    &image,
                                    store,
                                    limits.transfer,
                                    cancel,
                                )
                                .await?;
                                claim.check().await?;
                                let digest = saved
                                    .archive_digest()
                                    .ok_or(CondaImageError::MissingArchive)?
                                    .parse()
                                    .map_err(|error| {
                                        std::io::Error::other(format!(
                                            "invalid saved archive: {error}"
                                        ))
                                    })?;
                                let temporary = tempfile::NamedTempFile::new()?;
                                store
                                    .materialize_content_bounded(
                                        &digest,
                                        temporary.path(),
                                        limits.transfer.archive_bytes,
                                    )
                                    .await?;
                                cache.publish(preparation, &claim, &saved).await?
                            }
                        }
                    }
                }
            };
            match conda::restore(
                engine,
                preparation,
                cached.receipt(),
                store,
                limits.transfer,
                cancel,
            )
            .await
            {
                Ok(image) => {
                    if let Some(registry) = registry {
                        let published = conda::publish(
                            engine,
                            preparation,
                            &image,
                            registry,
                            limits.transfer,
                            cancel,
                        )
                        .await?;
                        store
                            .put_entry(
                                "conda-registry",
                                &key,
                                serde_json::to_vec(&published)
                                    .map_err(std::io::Error::other)?
                                    .into(),
                            )
                            .await?;
                        return Ok(published);
                    }
                    return Ok(image);
                }
                Err(
                    CondaImageError::Provenance(_)
                    | CondaImageError::MissingArchive
                    | CondaImageError::Store(StoreError::Corrupt { .. }),
                ) => {
                    cache.invalidate(preparation, &cached).await?;
                }
                Err(error) => return Err(error),
            }
        }
        Err(CondaImageError::Limit("cache attempts"))
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(CondaImageError::Cancelled),
        result = tokio::time::timeout(limits.timeout, work) => {
            result.map_err(|_| CondaImageError::Limit("cache wall-clock"))?
        }
    }
}

async fn local(
    engine: &ContainerEngine,
    preparation: &ImagePreparation,
    limit: usize,
) -> Result<Option<CompletedImage>, CondaImageError> {
    let options = ListImagesOptions {
        all: true,
        filters: Some(HashMap::from([(
            "label".into(),
            vec![format!(
                "org.iunctus.conda.preparation={}",
                preparation.digest()
            )],
        )])),
        ..ListImagesOptions::default()
    };
    let images = engine.client().list_images(Some(options)).await?;
    if images.len() > limit {
        return Err(CondaImageError::Limit("local image candidates"));
    }
    let mut ids: Vec<_> = images.into_iter().map(|image| image.id).collect();
    ids.sort();
    ids.dedup();
    for id in ids {
        match conda::inspect(engine, preparation, &id).await {
            Ok(image) => return Ok(Some(image)),
            Err(CondaImageError::Provenance(_)) => {}
            Err(CondaImageError::Engine(bollard::errors::Error::DockerResponseServerError {
                status_code: 404,
                ..
            })) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}
