//! Builds completed Conda images through the Docker-compatible classic build API.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::future::Future;
use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use bollard::auth::DockerCredentials;
use bollard::query_parameters::{
    BuildImageOptions, BuilderVersion, ImportImageOptions, PushImageOptions, TagImageOptions,
};
use conda_environment::{BuiltImage, CompletedImage, ImagePreparation};
use content_store::Store;
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

use crate::{CancellationToken, ContainerEngine};

const PREPARATION: &str = "org.iunctus.conda.preparation";
const BASE: &str = "org.iunctus.conda.base";
const RECIPE: &str = "org.iunctus.conda.recipe";

/// Finite context, progress, wall-clock and classic-builder resource allowances.
#[derive(Clone, Copy, Debug)]
pub struct BuildLimits {
    pub context_bytes: usize,
    pub progress_bytes: usize,
    pub timeout: Duration,
    /// Classic Docker build uses signed 32-bit byte allowances.
    pub memory_bytes: i32,
    /// CPU microseconds allowed in each 100,000 microsecond period.
    pub cpu_quota: i32,
}

#[derive(Debug, thiserror::Error)]
pub enum CondaImageError {
    #[error("Conda image preparation failed: {0}")]
    Io(#[from] io::Error),
    #[error("Conda image engine request failed: {0}")]
    Engine(#[from] bollard::errors::Error),
    #[error("Conda image content store failed: {0}")]
    Store(#[from] content_store::StoreError),
    #[error("Conda image provenance is invalid: {0}")]
    Provenance(String),
    #[error("missing Conda image has no stored archive")]
    MissingArchive,
    #[error("Conda image operation was cancelled")]
    Cancelled,
    #[error("Conda image operation exceeded its {0} limit")]
    Limit(&'static str),
    #[error("Conda image publication failed: {0}")]
    Publication(String),
}

/// Bounds exported and downloaded archive bytes, decoded import progress, and total time.
#[derive(Clone, Copy, Debug)]
pub struct TransferLimits {
    pub archive_bytes: u64,
    pub progress_bytes: usize,
    pub timeout: Duration,
}

/// Publishes an inspected image and records the engine's manifest identity, never a tag.
pub async fn publish(
    engine: &ContainerEngine,
    preparation: &ImagePreparation,
    receipt: &CompletedImage,
    registry: &str,
    limits: TransferLimits,
    cancel: &CancellationToken,
) -> Result<CompletedImage, CondaImageError> {
    transfer(limits, cancel, async {
        receipt.validate(preparation)?;
        inspect(engine, preparation, receipt.image_id()).await?;
        let parsed = crate::registry::Reference::parse(registry);
        let found =
            crate::credentials::lookup(&parsed.registry).map_err(CondaImageError::Publication)?;
        let mut credentials = DockerCredentials {
            serveraddress: Some(parsed.registry.clone()),
            ..Default::default()
        };
        match found.credential {
            Some(crate::credentials::Credential::Basic(username, password)) => {
                credentials.username = Some(username);
                credentials.password = Some(password);
            }
            Some(crate::credentials::Credential::Bearer(token)) => {
                credentials.registrytoken = Some(token)
            }
            Some(crate::credentials::Credential::Identity(token)) => {
                credentials.identitytoken = Some(token)
            }
            None if found.helper => return Err(CondaImageError::Publication(found.hint().into())),
            None => {}
        }
        let failed =
            || CondaImageError::Publication("engine push failed (details redacted)".into());
        // Image-specific tags prevent concurrent preparations from moving each other's tag.
        let tag = format!("conda-{}", receipt.image_id().trim_start_matches("sha256:"));
        engine
            .client()
            .tag_image(
                receipt.image_id(),
                Some(TagImageOptions {
                    repo: Some(registry.into()),
                    tag: Some(tag.clone()),
                }),
            )
            .await
            .map_err(|_| failed())?;
        let mut stream = engine.client().push_image(
            registry,
            Some(PushImageOptions {
                tag: Some(tag),
                ..Default::default()
            }),
            Some(credentials),
        );
        let mut used = 0usize;
        while let Some(step) = stream.next().await {
            let step = step.map_err(|_| failed())?;
            if step.error_detail.is_some() {
                return Err(failed());
            }
            used = used
                .checked_add(serde_json::to_vec(&step).map_err(io::Error::other)?.len())
                .filter(|size| *size <= limits.progress_bytes)
                .ok_or(CondaImageError::Limit("push progress"))?;
        }
        let image = engine
            .client()
            .inspect_image(receipt.image_id())
            .await
            .map_err(|_| failed())?;
        let reference = image
            .repo_digests
            .unwrap_or_default()
            .into_iter()
            .find_map(|reference| {
                let candidate = crate::registry::Reference::parse(&reference);
                if candidate.registry == parsed.registry
                    && candidate.repository == parsed.repository
                {
                    candidate
                        .digest
                        .map(|digest| format!("{registry}@{digest}"))
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                CondaImageError::Publication("engine gave no published manifest digest".into())
            })?;
        let digest = reference
            .rsplit_once('@')
            .map(|(_, digest)| digest.to_owned());
        let published = preparation.completed(BuiltImage {
            preparation: receipt.preparation().into(),
            base: receipt.base().into(),
            recipe_digest: receipt.recipe_digest().into(),
            platform: receipt.platform().into(),
            image_id: receipt.image_id().into(),
            archive_digest: receipt.archive_digest().map(str::to_owned),
            manifest_digest: digest,
            reference: Some(reference.clone()),
        })?;
        if !crate::registry::exists(&reference)
            .await
            .map_err(|error| CondaImageError::Publication(error.to_string()))?
        {
            return Err(CondaImageError::Publication(
                "published manifest is missing".into(),
            ));
        }
        Ok(published)
    })
    .await
}

/// Requires the exact digest-selected base to be local, without pulling it. A Docker image
/// ID is a configuration digest; RepoDigests supplies the requested manifest identity.
/// Callers may impose a shorter deadline or cancellation by dropping this read-only future.
pub async fn require_base(
    engine: &ContainerEngine,
    preparation: &ImagePreparation,
) -> Result<(), CondaImageError> {
    let image = tokio::time::timeout(
        Duration::from_secs(30),
        engine.client().inspect_image(preparation.base()),
    )
    .await
    .map_err(|_| CondaImageError::Limit("base inspection wall-clock"))??;
    let (_, digest) = preparation
        .base()
        .rsplit_once('@')
        .ok_or_else(|| invalid("unpinned base"))?;
    if !image.repo_digests.as_ref().is_some_and(|references| {
        references.iter().any(|reference| {
            reference
                .rsplit_once('@')
                .is_some_and(|(_, found)| found == digest)
        })
    }) {
        return Err(CondaImageError::Provenance(
            "base inspection does not contain the requested manifest digest".into(),
        ));
    }
    let mut platform = format!(
        "{}/{}",
        image.os.unwrap_or_default(),
        image.architecture.unwrap_or_default()
    );
    if let Some(variant) = image.variant.filter(|variant| !variant.is_empty()) {
        platform.push('/');
        platform.push_str(&variant);
    }
    if platform != preparation.platform() {
        return Err(CondaImageError::Provenance(format!(
            "base platform {platform} differs from {}",
            preparation.platform()
        )));
    }
    Ok(())
}

/// Saves an inspected image by its immutable ID. Only the archive digest is added; saving
/// does not publish a registry manifest. Cancellation may leave unreferenced content bytes.
pub async fn save(
    engine: &ContainerEngine,
    preparation: &ImagePreparation,
    receipt: &CompletedImage,
    store: &Store,
    limits: TransferLimits,
    cancel: &CancellationToken,
) -> Result<CompletedImage, CondaImageError> {
    transfer(limits, cancel, async {
        receipt.validate(preparation)?;
        let image = inspect(engine, preparation, receipt.image_id()).await?;
        let temporary = tempfile::NamedTempFile::new()?;
        let mut file = tokio::fs::File::from_std(temporary.reopen()?);
        let mut bytes = 0u64;
        let mut stream = engine.client().export_image(image.image_id());
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            bytes = bytes
                .checked_add(chunk.len() as u64)
                .filter(|size| *size <= limits.archive_bytes)
                .ok_or(CondaImageError::Limit("archive bytes"))?;
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        let (digest, _) = store.put_file(temporary.path()).await?;
        Ok(preparation.completed(BuiltImage {
            preparation: image.preparation().into(),
            base: image.base().into(),
            recipe_digest: image.recipe_digest().into(),
            platform: image.platform().into(),
            image_id: image.image_id().into(),
            manifest_digest: None,
            archive_digest: Some(digest.to_string()),
            reference: None,
        })?)
    })
    .await
}

/// Reuses a verified local ID, or imports checksum-verified stored bytes when that exact
/// ID is missing. Import success is accepted only after inspecting the expected ID again.
/// Cancellation closes the request but cannot confirm daemon-side import termination.
pub async fn restore(
    engine: &ContainerEngine,
    preparation: &ImagePreparation,
    receipt: &CompletedImage,
    store: &Store,
    limits: TransferLimits,
    cancel: &CancellationToken,
) -> Result<CompletedImage, CondaImageError> {
    transfer(limits, cancel, async {
        receipt.validate(preparation)?;
        match inspect(engine, preparation, receipt.image_id()).await {
            Ok(_) => return Ok(receipt.clone()),
            Err(CondaImageError::Engine(bollard::errors::Error::DockerResponseServerError {
                status_code: 404,
                ..
            })) => {}
            Err(error) => return Err(error),
        }
        let digest: task_model::Digest = receipt
            .archive_digest()
            .ok_or(CondaImageError::MissingArchive)?
            .parse()
            .map_err(|error| invalid(&format!("invalid archive digest: {error}")))?;
        let temporary = tempfile::NamedTempFile::new()?;
        store
            .materialize_content_bounded(&digest, temporary.path(), limits.archive_bytes)
            .await?;
        let file = tokio::fs::File::open(temporary.path()).await?;
        // The pinned client cannot encode platform arrays; inspection below checks platform.
        let mut stream = engine.client().import_image(
            ImportImageOptions::default(),
            bollard::body_try_stream(ReaderStream::new(file)),
            None,
        );
        let mut used = 0usize;
        while let Some(step) = stream.next().await {
            let size = format!("{:?}", step?).len();
            used = used
                .checked_add(size)
                .filter(|size| *size <= limits.progress_bytes)
                .ok_or(CondaImageError::Limit("import progress"))?;
        }
        inspect(engine, preparation, receipt.image_id()).await?;
        Ok(receipt.clone())
    })
    .await
}

async fn transfer<T>(
    limits: TransferLimits,
    cancel: &CancellationToken,
    work: impl Future<Output = Result<T, CondaImageError>>,
) -> Result<T, CondaImageError> {
    if limits.archive_bytes == 0
        || limits.progress_bytes == 0
        || limits.timeout.is_zero()
        || Instant::now().checked_add(limits.timeout).is_none()
    {
        return Err(invalid("transfer limits must be positive and finite"));
    }
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(CondaImageError::Cancelled),
        result = tokio::time::timeout(limits.timeout, work) => {
            result.map_err(|_| CondaImageError::Limit("wall-clock"))?
        }
    }
}

/// Builds with the classic builder only. Engines requiring BuildKit are unsupported.
/// Cancelling or timing out closes the request and returns no receipt; daemon-side build
/// termination is engine-dependent and is not confirmed by this API.
pub async fn build(
    engine: &ContainerEngine,
    preparation: &ImagePreparation,
    package_cache: &Path,
    limits: BuildLimits,
    cancel: &CancellationToken,
) -> Result<CompletedImage, CondaImageError> {
    if limits.context_bytes == 0
        || limits.progress_bytes == 0
        || limits.memory_bytes <= 0
        || limits.cpu_quota <= 0
        || limits.timeout.is_zero()
    {
        return Err(invalid("build limits must be positive"));
    }
    let deadline = Instant::now()
        .checked_add(limits.timeout)
        .ok_or_else(|| invalid("build timeout is too large"))?;
    let operation = cancel.child_token();
    let _guard = operation.clone().drop_guard();
    let work = async {
        let prepared = preparation.clone();
        let cache = package_cache.to_owned();
        let cancellation = operation.clone();
        let context = tokio::task::spawn_blocking(move || {
            let mut out = Context {
                bytes: Vec::new(),
                limit: limits.context_bytes,
                deadline,
                cancellation,
            };
            prepared.write_context(&cache, &mut out)?;
            Ok::<_, io::Error>(out.bytes)
        })
        .await
        .map_err(|error| invalid(&error.to_string()))??;
        let options = BuildImageOptions {
            labels: Some(HashMap::from([
                (PREPARATION.into(), preparation.digest().into()),
                (BASE.into(), preparation.base().into()),
                (RECIPE.into(), preparation.recipe_digest()),
            ])),
            platform: preparation.platform().into(),
            version: BuilderVersion::BuilderV1,
            networkmode: Some("none".into()),
            memory: Some(limits.memory_bytes),
            memswap: Some(limits.memory_bytes),
            cpuperiod: Some(100_000),
            cpuquota: Some(limits.cpu_quota),
            forcerm: true,
            ..BuildImageOptions::default()
        };
        let mut progress =
            engine
                .client()
                .build_image(options, None, Some(bollard::body_full(context.into())));
        let mut used = 0usize;
        let mut image_id = None;
        while let Some(step) = progress.next().await {
            let step = step?;
            let size = format!("{step:?}").len();
            used = used
                .checked_add(size)
                .filter(|used| *used <= limits.progress_bytes)
                .ok_or(CondaImageError::Limit("build progress"))?;
            if let Some(id) = step.aux.and_then(|aux| aux.id) {
                image_id = Some(id);
            } else if let Some(stream) = step.stream {
                for line in stream.lines() {
                    if let Some(id) = line.strip_prefix("Successfully built ") {
                        let id = id.trim();
                        if id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                            image_id = Some(format!("sha256:{id}"));
                        }
                    }
                }
            }
        }
        let id = image_id.ok_or_else(|| invalid("builder returned no full immutable image ID"))?;
        inspect(engine, preparation, &id).await
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(CondaImageError::Cancelled),
        result = tokio::time::timeout(limits.timeout, work) => {
            result.map_err(|_| CondaImageError::Limit("wall-clock"))?
        }
    }
}

pub(crate) async fn inspect(
    engine: &ContainerEngine,
    preparation: &ImagePreparation,
    id: &str,
) -> Result<CompletedImage, CondaImageError> {
    if !id.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    }) {
        return Err(CondaImageError::Provenance(
            "invalid immutable image ID".into(),
        ));
    }
    let image = engine.client().inspect_image(id).await?;
    if image.id.as_deref() != Some(id) {
        return Err(CondaImageError::Provenance(
            "inspected image ID differs from the completed build".into(),
        ));
    }
    let mut platform = format!(
        "{}/{}",
        image.os.unwrap_or_default(),
        image.architecture.unwrap_or_default()
    );
    if let Some(variant) = image.variant.filter(|variant| !variant.is_empty()) {
        platform.push('/');
        platform.push_str(&variant);
    }
    let labels = image
        .config
        .and_then(|config| config.labels)
        .unwrap_or_default();
    preparation
        .completed(BuiltImage {
            preparation: labels.get(PREPARATION).cloned().unwrap_or_default(),
            base: labels.get(BASE).cloned().unwrap_or_default(),
            recipe_digest: labels.get(RECIPE).cloned().unwrap_or_default(),
            platform,
            image_id: id.into(),
            manifest_digest: None,
            archive_digest: None,
            reference: None,
        })
        .map_err(|error| CondaImageError::Provenance(error.to_string()))
}

fn invalid(message: &str) -> CondaImageError {
    io::Error::new(io::ErrorKind::InvalidData, message).into()
}

struct Context {
    bytes: Vec<u8>,
    limit: usize,
    deadline: Instant,
    cancellation: CancellationToken,
}

impl Write for Context {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.cancellation.is_cancelled() || Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "context cancelled or timed out",
            ));
        }
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("build context exceeds its byte limit"));
        }
        self.bytes
            .try_reserve_exact(bytes.len())
            .map_err(io::Error::other)?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
